/**
 * Opt-in, hermetic pinned-source Pi coding-profile adapter.
 *
 * It imports the pinned Agent core plus the coding-agent's own default system
 * prompt and `read`/`bash`/`edit`/`write` factories. It never invokes a host
 * `pi` executable or TUI. The pinned shallow source deliberately omits a
 * generated model-catalog file required merely to import `createAgentSession`;
 * using the focused public profile factories below keeps catalog hydration out
 * of this semantic/coding evaluation while preserving the exact active prompt
 * and tool surface under test.
 */

import { deepStrictEqual } from "node:assert";
import { execFileSync } from "node:child_process";
import { readFile, writeFile } from "node:fs/promises";
import { Agent } from "../parity/upstream/source/packages/agent/src/agent.ts";
import { stream as openAICompletionsStream } from "../parity/upstream/source/packages/ai/src/api/openai-completions.ts";
import { getDocsPath, getExamplesPath, getReadmePath } from "../parity/upstream/source/packages/coding-agent/src/config.ts";
import { buildSystemPrompt } from "../parity/upstream/source/packages/coding-agent/src/core/system-prompt.ts";
import {
	createCodingToolDefinitions,
	createCodingTools,
	type BashSpawnContext,
} from "../parity/upstream/source/packages/coding-agent/src/core/tools/index.ts";

const RESULT_SCHEMA = "pi-coding-eval-result/v0";
const UPSTREAM_COMMIT = "9d2ec7ffabe927bfad2214c1cee25b6632a78dcf";

type Args = {
	model: string;
	taskJson: string;
	workspace: string;
	capabilitiesJson: string;
	resultJson: string;
	attemptId: string;
	baselineId: string;
};

function parseArgs(argv: string[]): Args {
	const values = new Map<string, string>();
	for (let index = 0; index < argv.length; index += 2) {
		const flag = argv[index];
		const value = argv[index + 1];
		if (!flag?.startsWith("--") || !value || values.has(flag)) {
			throw new Error("expected one value for each explicit evaluation adapter flag");
		}
		values.set(flag, value);
	}
	const take = (flag: string): string => {
		const value = values.get(flag);
		if (!value) throw new Error(`missing required argument ${flag}`);
		return value;
	};
	return {
		model: take("--model"),
		taskJson: take("--task-json"),
		workspace: take("--workspace"),
		capabilitiesJson: take("--capabilities-json"),
		resultJson: take("--result-json"),
		attemptId: take("--attempt-id"),
		baselineId: take("--baseline-id"),
	};
}

/** Keep the Vault credential in the model callback, never in a tool child. */
function isolateProcessEnvironment(): void {
	const allowed = new Set(["PATH", "LANG", "LC_ALL", "TMPDIR", "TMP", "TEMP"]);
	for (const key of Object.keys(process.env)) {
		if (!allowed.has(key)) delete process.env[key];
	}
	process.env.LANG = "C";
	process.env.LC_ALL = "C";
}

function assertPinnedSource(): void {
	const commit = execFileSync("git", ["rev-parse", "HEAD"], { cwd: process.cwd(), encoding: "utf8" }).trim();
	if (commit !== UPSTREAM_COMMIT) throw new Error(`upstream source is not pinned at ${UPSTREAM_COMMIT}`);
	const dirty = execFileSync("git", ["status", "--porcelain", "--untracked-files=no"], {
		cwd: process.cwd(),
		encoding: "utf8",
	}).trim();
	if (dirty) throw new Error("pinned upstream source has tracked modifications");
}

function pinnedDefaultSystemPrompt(workspace: string): string {
	const definitions = createCodingToolDefinitions(workspace);
	const prompt = buildSystemPrompt({
		cwd: workspace,
		selectedTools: definitions.map((definition) => definition.name),
		toolSnippets: Object.fromEntries(definitions.map((definition) => [definition.name, definition.promptSnippet])),
		promptGuidelines: definitions.flatMap((definition) => definition.promptGuidelines ?? []),
	});
	// Keep Pi's source-built default prompt byte-identical to the captured Rust
	// profile. These are only installation-specific documentation locations,
	// lifted to the profile's fixed virtual paths; the prompt template, selected
	// tools, snippets, guidelines, and caller workspace remain exactly Pi's.
	return prompt
		.replaceAll(getReadmePath(), "/fixture/pi/README.md")
		.replaceAll(getDocsPath(), "/fixture/pi/docs")
		.replaceAll(getExamplesPath(), "/fixture/pi/examples");
}

function redactShellEnvironment(context: BashSpawnContext): BashSpawnContext {
	const env = { ...context.env };
	delete env.OPENROUTER_API_KEY;
	delete env.OPENAI_API_KEY;
	delete env.ANTHROPIC_API_KEY;
	delete env.GITHUB_TOKEN;
	return { ...context, env };
}

function terminalStatus(stopReason: string): "completed" | "failed" | "cancelled" | "aborted" {
	if (stopReason === "error") return "failed";
	if (stopReason === "aborted") return "aborted";
	return "completed";
}

async function main(): Promise<void> {
	const args = parseArgs(process.argv.slice(2));
	const apiKey = process.env.OPENROUTER_API_KEY;
	if (!apiKey) throw new Error("OPENROUTER_API_KEY must be supplied by the caller's secret injector");
	assertPinnedSource();
	isolateProcessEnvironment();
	const task = JSON.parse(await readFile(args.taskJson, "utf8")) as { prompt?: unknown; capabilities?: unknown };
	const capabilities = JSON.parse(await readFile(args.capabilitiesJson, "utf8"));
	deepStrictEqual(capabilities, task.capabilities, "evaluation capability manifest does not match task");
	if (typeof task.prompt !== "string" || task.prompt.length === 0) throw new Error("evaluation task has no prompt");

	const tools = createCodingTools(args.workspace, { bash: { spawnHook: redactShellEnvironment } });
	const model = {
		id: args.model,
		name: args.model,
		api: "openai-completions",
		provider: "openrouter",
		baseUrl: "https://openrouter.ai/api/v1",
		reasoning: false,
		input: ["text"],
		cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
		contextWindow: 131072,
		maxTokens: 1024,
	} as const;
	const eventTypes: string[] = [];
	let toolCalls = 0;
	const agent = new Agent({
		streamFn: openAICompletionsStream as never,
		getApiKey: (provider) => (provider === "openrouter" ? apiKey : undefined),
		initialState: {
			systemPrompt: pinnedDefaultSystemPrompt(args.workspace),
			model: model as never,
			thinkingLevel: "off",
			tools,
		},
	});
	agent.subscribe((event) => {
		eventTypes.push(event.type);
		if (event.type === "tool_execution_start") toolCalls += 1;
	});
	await agent.prompt(task.prompt);
	const assistant = [...agent.state.messages].reverse().find((message) => message.role === "assistant");
	if (!assistant || assistant.role !== "assistant") {
		throw new Error("pinned coding profile did not settle with an assistant response");
	}
	const finalText = assistant.content
		.filter((part) => part.type === "text")
		.map((part) => part.text)
		.join("");
	const output = {
		schema_version: RESULT_SCHEMA,
		attempt_id: args.attemptId,
		baseline_id: args.baselineId,
		terminal: { status: terminalStatus(assistant.stopReason) },
		final_text: finalText,
		turns: eventTypes.filter((type) => type === "turn_start").length,
		tool_calls: toolCalls,
		usage: {
			input: assistant.usage.input,
			output: assistant.usage.output,
			cache_read: assistant.usage.cacheRead,
			cache_write: assistant.usage.cacheWrite,
		},
		trace: eventTypes.map((type, seq) => ({ seq, type })),
	};
	await writeFile(args.resultJson, `${JSON.stringify(output)}\n`, "utf8");
}

main().catch((error: unknown) => {
	process.stderr.write(`pinned upstream coding-profile adapter: ${error instanceof Error ? error.message : String(error)}\n`);
	process.exitCode = 2;
});
