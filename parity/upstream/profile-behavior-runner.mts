/**
 * Run provider-free, direct standard-tool behavior cases against the pinned coding-agent
 * factories.
 *
 * This runner intentionally calls tool factories and `validateToolArguments` directly.  It does
 * not construct an Agent, start a model stream, invoke the Pi CLI, read a real workspace, spawn a
 * shell, or contact a provider.  The virtual operation adapters below are the only side-effect
 * boundary for the executable cases.
 *
 * Run from `parity/upstream/source` after the pinned dependencies are installed:
 *
 *   PI_OFFLINE=1 ./node_modules/.bin/tsx ../profile-behavior-runner.mts > ../../profile/behavior-capture.json
 *
 * The command exits 2 when a pinned factory cannot be isolated behind its documented operation
 * adapter.  At the pinned commit, grep is intentionally reported this way: its implementation
 * calls `ensureTool("rg")` and spawns ripgrep before it uses the injected `GrepOperations`, so a
 * virtual adapter cannot provide an exact provider-free success or host-error execution.
 */

import { validateToolArguments } from "./source/packages/ai/src/index.ts";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { AgentTool } from "./source/packages/agent/src/types.ts";
import {
	createBashTool,
	createEditTool,
	createFindTool,
	createGrepTool,
	createLsTool,
	createReadTool,
	createWriteTool,
	type BashOperations,
	type EditOperations,
	type FindOperations,
	type GrepOperations,
	type LsOperations,
	type ReadOperations,
	type WriteOperations,
} from "./source/packages/coding-agent/src/core/tools/index.ts";

const cwd = "/fixture/workspace";
const callId = "profile-behavior-call";

type CaseKind = "success" | "invalid_input" | "host_error";
type CaseStatus = CaseKind | "blocked";

type OperationCall = {
	method: string;
	path?: string;
	command?: string;
	pattern?: string;
};

type VirtualState = {
	files: Map<string, string>;
	directories: Set<string>;
	calls: OperationCall[];
	failMethod?: string;
};

type CapturedCase = {
	id: string;
	kind: CaseKind;
	status: CaseStatus;
	input: unknown;
	calls: OperationCall[];
	result?: {
		content: unknown;
		details: unknown;
		usage?: unknown;
		terminate?: boolean;
		added_tool_names?: string[];
	};
	error?: string;
	blocker?: string;
};

type CapturedTool = {
	name: string;
	source: string;
	cases: CapturedCase[];
};

function absolute(path: string | undefined): string {
	if (!path || path === ".") return cwd;
	if (path.startsWith("/")) return path;
	return `${cwd}/${path}`;
}

function virtualState(failMethod?: string): VirtualState {
	return {
		files: new Map([
			[`${cwd}/notes.txt`, "needle\nsecond line\n"],
			[`${cwd}/src/lib.rs`, "fn main() {\n  old\n}\n"],
		]),
		directories: new Set([cwd, `${cwd}/src`]),
		calls: [],
		failMethod,
	};
}

function record(state: VirtualState, call: OperationCall): void {
	state.calls.push(call);
	if (state.failMethod === call.method) {
		throw new Error(`virtual ${call.method} failure`);
	}
}

function readOperations(state: VirtualState): ReadOperations {
	return {
		access: async (path) => {
			record(state, { method: "access", path });
			if (!state.files.has(path)) throw new Error(`missing virtual file: ${path}`);
		},
		readFile: async (path) => {
			record(state, { method: "readFile", path });
			const content = state.files.get(path);
			if (content === undefined) throw new Error(`missing virtual file: ${path}`);
			return Buffer.from(content, "utf8");
		},
		detectImageMimeType: async (path) => {
			record(state, { method: "detectImageMimeType", path });
			return undefined;
		},
	};
}

function bashOperations(state: VirtualState): BashOperations {
	return {
		exec: async (command, path, options) => {
			record(state, { method: "exec", path, command });
			options.onData(Buffer.from("ok", "utf8"));
			return { exitCode: 0 };
		},
	};
}

function editOperations(state: VirtualState): EditOperations {
	return {
		access: async (path) => {
			record(state, { method: "access", path });
			if (!state.files.has(path)) throw new Error(`missing virtual file: ${path}`);
		},
		readFile: async (path) => {
			record(state, { method: "readFile", path });
			const content = state.files.get(path);
			if (content === undefined) throw new Error(`missing virtual file: ${path}`);
			return Buffer.from(content, "utf8");
		},
		writeFile: async (path, content) => {
			record(state, { method: "writeFile", path });
			state.files.set(path, content);
		},
	};
}

function writeOperations(state: VirtualState): WriteOperations {
	return {
		mkdir: async (path) => {
			record(state, { method: "mkdir", path });
			state.directories.add(path);
		},
		writeFile: async (path, content) => {
			record(state, { method: "writeFile", path });
			state.files.set(path, content);
		},
	};
}

function grepOperations(state: VirtualState): GrepOperations {
	return {
		isDirectory: (path) => {
			record(state, { method: "isDirectory", path });
			if (!state.directories.has(path)) throw new Error(`missing virtual directory: ${path}`);
			return true;
		},
		readFile: (path) => {
			record(state, { method: "readFile", path });
			return state.files.get(path) ?? "";
		},
	};
}

function findOperations(state: VirtualState): FindOperations {
	return {
		exists: (path) => {
			record(state, { method: "exists", path });
			if (!state.directories.has(path)) throw new Error(`missing virtual directory: ${path}`);
			return true;
		},
		glob: (pattern, path) => {
			record(state, { method: "glob", path, pattern });
			return [`${path}/lib.rs`];
		},
	};
}

function lsOperations(state: VirtualState): LsOperations {
	return {
		exists: (path) => {
			record(state, { method: "exists", path });
			if (!state.directories.has(path)) throw new Error(`missing virtual directory: ${path}`);
			return true;
		},
		stat: (path) => {
			record(state, { method: "stat", path });
			return { isDirectory: () => state.directories.has(path) };
		},
		readdir: (path) => {
			record(state, { method: "readdir", path });
			return ["lib.rs"];
		},
	};
}

function toolCall(name: string, argumentsValue: unknown): { name: string; arguments: unknown } {
	return { name, arguments: argumentsValue };
}

function errorText(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

function resultShape(result: any): CapturedCase["result"] {
	return {
		content: result.content,
		details: result.details ?? null,
		...(result.usage === undefined ? {} : { usage: result.usage }),
		...(result.terminate === undefined ? {} : { terminate: result.terminate }),
		...(result.addedToolNames === undefined ? {} : { added_tool_names: result.addedToolNames }),
	};
}

async function executeCase(
	tool: AgentTool,
	name: string,
	id: string,
	input: unknown,
	state: VirtualState,
	kind: CaseKind = "success",
): Promise<CapturedCase> {
	const captured: CapturedCase = { id, kind, status: kind, input, calls: state.calls };
	try {
		const args = validateToolArguments(tool as any, toolCall(name, input) as any);
		const result = await tool.execute(callId, args, undefined, undefined);
		captured.result = resultShape(result);
		return captured;
	} catch (error) {
		captured.error = errorText(error);
		return captured;
	}
}

async function invalidCase(
	tool: AgentTool,
	name: string,
	id: string,
	input: unknown,
	state: VirtualState,
): Promise<CapturedCase> {
	const captured: CapturedCase = {
		id,
		kind: "invalid_input",
		status: "invalid_input",
		input,
		calls: state.calls,
	};
	try {
		const args = validateToolArguments(tool as any, toolCall(name, input) as any);
		await tool.execute(callId, args, undefined, undefined);
		captured.status = "success";
		captured.error = "invalid input unexpectedly succeeded";
	} catch (error) {
		captured.error = errorText(error);
	}
	return captured;
}

function blockedCase(id: string, input: unknown, reason: string): CapturedCase {
	return {
		id,
		kind: id.includes("invalid") ? "invalid_input" : id.includes("host") ? "host_error" : "success",
		status: "blocked",
		input,
		calls: [],
		blocker: reason,
	};
}

export const grepBlocker =
	'Pinned grep executes ensureTool("rg") before invoking injected GrepOperations, then spawns ripgrep; a virtual adapter cannot provide provider-free exact search behavior without changing the pinned source or adding an external process seam.';

export async function capture(): Promise<{
	format_version: number;
	kind: string;
	upstream: object;
	tools: CapturedTool[];
	blockers: string[];
}> {
	const tools: CapturedTool[] = [];

	const readState = virtualState();
	const readHostState = virtualState("access");
	const readHostTool = createReadTool(cwd, { operations: readOperations(readHostState) });
	const read = createReadTool(cwd, { operations: readOperations(readState) });
	tools.push({
		name: "read",
		source: "packages/coding-agent/src/core/tools/read.ts:createReadTool",
		cases: [
			await executeCase(read, "read", "success-text", { path: "notes.txt" }, readState),
			await invalidCase(read, "read", "invalid-input", { path: {} }, virtualState()),
			await executeCase(readHostTool, "read", "host-read-failure", { path: "notes.txt" }, readHostState, "host_error"),
		],
	});

	const bashState = virtualState();
	const bashHostState = virtualState("exec");
	const bashHostTool = createBashTool(cwd, { operations: bashOperations(bashHostState), exposeSessionEnvironment: false });
	const bash = createBashTool(cwd, { operations: bashOperations(bashState), exposeSessionEnvironment: false });
	tools.push({
		name: "bash",
		source: "packages/coding-agent/src/core/tools/bash.ts:createBashTool",
		cases: [
			await executeCase(bash, "bash", "success-command", { command: "printf ok" }, bashState),
			await invalidCase(bash, "bash", "invalid-timeout", { command: {}, timeout: -1 }, virtualState()),
			await executeCase(bashHostTool, "bash", "host-command-failure", { command: "printf ok" }, bashHostState, "host_error"),
		],
	});

	const editState = virtualState();
	const editHostState = virtualState("access");
	const editHostTool = createEditTool(cwd, { operations: editOperations(editHostState) });
	const edit = createEditTool(cwd, { operations: editOperations(editState) });
	tools.push({
		name: "edit",
		source: "packages/coding-agent/src/core/tools/edit.ts:createEditTool",
		cases: [
			await executeCase(edit, "edit", "success-exact-replacement", { path: "src/lib.rs", edits: [{ oldText: "old", newText: "new" }] }, editState),
			await invalidCase(edit, "edit", "invalid-empty-edits", { path: "src/lib.rs", edits: [] }, virtualState()),
			await executeCase(editHostTool, "edit", "host-write-failure", { path: "src/lib.rs", edits: [{ oldText: "old", newText: "new" }] }, editHostState, "host_error"),
		],
	});

	const writeState = virtualState();
	const writeHostState = virtualState("writeFile");
	const writeHostTool = createWriteTool(cwd, { operations: writeOperations(writeHostState) });
	const write = createWriteTool(cwd, { operations: writeOperations(writeState) });
	tools.push({
		name: "write",
		source: "packages/coding-agent/src/core/tools/write.ts:createWriteTool",
		cases: [
			await executeCase(write, "write", "success-file", { path: "out/result.txt", content: "ok" }, writeState),
			await invalidCase(write, "write", "invalid-missing-content", { path: "out/result.txt" }, virtualState()),
			await executeCase(writeHostTool, "write", "host-write-failure", { path: "out/result.txt", content: "ok" }, writeHostState, "host_error"),
		],
	});

	const grepState = virtualState();
	const grep = createGrepTool(cwd, { operations: grepOperations(grepState) });
	tools.push({
		name: "grep",
		source: "packages/coding-agent/src/core/tools/grep.ts:createGrepTool",
		cases: [
            blockedCase("success-match", { pattern: "needle", path: "src" }, grepBlocker),
			await invalidCase(grep, "grep", "invalid-missing-pattern", { path: "src" }, virtualState()),
            blockedCase("host-search-failure", { pattern: "needle", path: "missing" }, grepBlocker),
		],
	});

	const findState = virtualState();
	const findHostState = virtualState("exists");
	const findHostTool = createFindTool(cwd, { operations: findOperations(findHostState) });
	const find = createFindTool(cwd, { operations: findOperations(findState) });
	tools.push({
		name: "find",
		source: "packages/coding-agent/src/core/tools/find.ts:createFindTool",
		cases: [
			await executeCase(find, "find", "success-glob", { pattern: "**/*.rs", path: "src" }, findState),
			await invalidCase(find, "find", "invalid-missing-pattern", { path: "src" }, virtualState()),
			await executeCase(findHostTool, "find", "host-glob-failure", { pattern: "**/*.rs", path: "src" }, findHostState, "host_error"),
		],
	});

	const lsState = virtualState();
	const lsHostState = virtualState("exists");
	const lsHostTool = createLsTool(cwd, { operations: lsOperations(lsHostState) });
	const ls = createLsTool(cwd, { operations: lsOperations(lsState) });
	tools.push({
		name: "ls",
		source: "packages/coding-agent/src/core/tools/ls.ts:createLsTool",
		cases: [
			await executeCase(ls, "ls", "success-directory", { path: "src" }, lsState),
			await invalidCase(ls, "ls", "invalid-path", { path: {} }, virtualState()),
			await executeCase(lsHostTool, "ls", "host-directory-failure", { path: "src" }, lsHostState, "host_error"),
		],
	});

	return {
		format_version: 1,
		kind: "pinned_profile_behavior_capture",
		upstream: {
			repository: "https://github.com/earendil-works/pi.git",
			commit: "9d2ec7ffabe927bfad2214c1cee25b6632a78dcf",
			package: "@earendil-works/pi-coding-agent",
			package_version: "0.84.1",
		},
		tools,
		blockers: [grepBlocker],
	};
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
	const captureResult = await capture();
	process.stdout.write(`${JSON.stringify(captureResult, null, 2)}\n`);

	const blocked = captureResult.tools.flatMap((tool) => tool.cases.filter((test) => test.status === "blocked"));
	if (blocked.length > 0) {
		process.exitCode = 2;
	}
}
