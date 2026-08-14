/**
 * Capture the pinned `grep` factory through its unavoidable ripgrep process seam.
 *
 * This is deliberately separate from `profile-behavior-runner.mts`: the other runner proves
 * operation-adapter isolation, while upstream grep resolves and spawns `rg` before it consults
 * `GrepOperations`.  The runner creates both an empty `PI_CODING_AGENT_DIR` and a disposable
 * workspace *before* dynamically importing the pinned source.  Consequently `ensureTool("rg")`
 * can resolve only the process PATH (never a managed binary beneath the host's `~/.pi`), and the
 * pinned source still owns the actual search behavior.
 *
 * It imports source modules in-process; it never invokes the Pi executable, builds an Agent, or
 * contacts a provider.  A missing `rg` is a hard verification failure rather than a download
 * attempt (`PI_OFFLINE=1`).
 */

import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

type CapturedCase = {
	id: string;
	status: "success" | "invalid_input" | "host_error";
	input: unknown;
	content?: unknown;
	details?: unknown;
	error?: string;
};

type SourceTool = {
	execute: (callId: string, argumentsValue: unknown, signal?: AbortSignal, onUpdate?: unknown) => Promise<unknown>;
};

function text(error: unknown, workspace: string): string {
	return (error instanceof Error ? error.message : String(error)).replaceAll(workspace, "/fixture/workspace");
}

function resultShape(result: any): Pick<CapturedCase, "content" | "details"> {
	return {
		content: result.content,
		details: result.details ?? null,
	};
}

/** Run the three behavior cases in a process whose Pi-managed binary directory is empty. */
export async function capture(): Promise<{
	format_version: number;
	kind: string;
	rg_path: string;
	tools: Array<{ name: string; cases: CapturedCase[] }>;
}> {
	const root = await mkdtemp(join(tmpdir(), "pi-profile-grep-process-"));
	const workspace = join(root, "workspace");
	const agentDirectory = join(root, "empty-agent-directory");

	try {
		await mkdir(workspace);
		await mkdir(agentDirectory);
		await writeFile(join(workspace, "notes.txt"), "before\nneedle\nneedle suffix\nafter\n", "utf8");

		// `config.ts` derives its managed binary directory when it is first evaluated, so this must
		// precede every pinned source import below.
		process.env.PI_CODING_AGENT_DIR = agentDirectory;
		process.env.PI_OFFLINE = "1";

		const [{ createGrepTool }, { validateToolArguments }, { getToolPath }] = await Promise.all([
			import("./source/packages/coding-agent/src/core/tools/index.ts"),
			import("./source/packages/ai/src/index.ts"),
			import("./source/packages/coding-agent/src/utils/tools-manager.ts"),
		]);
		const rgPath = getToolPath("rg");
		if (rgPath !== "rg") {
			throw new Error(`expected isolated pinned grep to resolve PATH rg, got ${String(rgPath)}`);
		}

		const tool = createGrepTool(workspace) as SourceTool;
		const execute = async (id: string, input: unknown): Promise<CapturedCase> => {
			try {
				const argumentsValue = validateToolArguments(tool as any, {
					name: "grep",
					arguments: input,
				} as any);
				const result = await tool.execute(`profile-grep-${id}`, argumentsValue);
				return { id, status: "success", input, ...resultShape(result) };
			} catch (error) {
				return { id, status: "host_error", input, error: text(error, workspace) };
			}
		};

		const invalidInput = { path: "notes.txt" };
		const invalid: CapturedCase = { id: "invalid-missing-pattern", status: "invalid_input", input: invalidInput };
		try {
			validateToolArguments(tool as any, { name: "grep", arguments: invalidInput } as any);
			invalid.status = "success";
			invalid.error = "invalid input unexpectedly succeeded";
		} catch (error) {
			invalid.error = text(error, workspace);
		}

		const success = await execute("success-context", {
			pattern: "needle",
			path: "notes.txt",
			context: 1,
		});
		const missing = await execute("host-missing-path", { pattern: "needle", path: "missing" });
		if (missing.status === "success") {
			missing.status = "host_error";
			missing.error = "missing path unexpectedly succeeded";
			delete missing.content;
			delete missing.details;
		}

		return {
			format_version: 1,
			kind: "pinned_profile_grep_process_capture",
			rg_path: rgPath,
			tools: [{ name: "grep", cases: [success, invalid, missing] }],
		};
	} finally {
		await rm(root, { recursive: true, force: true });
	}
}
