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

/** Keep the injected credential in the model callback, never in a tool child. */
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

type ProviderCostTurn = {
	turn: number;
	source: "openrouter_generation" | "openrouter_stream_usage" | "unavailable";
	total_usd: number | null;
	upstream_inference_usd: number | null;
	model: string;
	provider: string | null;
	input_tokens: number;
	output_tokens: number;
	cache_read_tokens: number;
	cache_write_tokens: number;
	reasoning_tokens: number | null;
};

function nonnegativeNumber(value: unknown): number | null {
	return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : null;
}

function token(value: unknown): number {
	return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : 0;
}

type StreamUsageCost = {
	total_usd: number;
	model: string | null;
	input_tokens: number;
	output_tokens: number;
	cache_read_tokens: number;
	cache_write_tokens: number;
	reasoning_tokens: number | null;
};

/**
 * Consume a clone of Pi's streaming response and retain only OpenRouter usage
 * fields. The parsed SSE payload is transient: completion content, response
 * identifiers, and raw provider objects are never appended to artifacts.
 */
async function streamUsageCost(response: Response): Promise<StreamUsageCost | null> {
	const body = response.body;
	if (!body) return null;
	const reader = body.getReader();
	const decoder = new TextDecoder();
	let buffer = "";
	let latest: StreamUsageCost | null = null;
	const inspect = (line: string) => {
		if (!line.startsWith("data:")) return;
		const payload = line.slice(5).trim();
		if (!payload || payload === "[DONE]") return;
		try {
			const chunk: unknown = JSON.parse(payload);
			if (!chunk || typeof chunk !== "object") return;
			const row = chunk as { usage?: unknown; model?: unknown };
			if (!row.usage || typeof row.usage !== "object") return;
			const usage = row.usage as Record<string, unknown>;
			const total = nonnegativeNumber(usage.cost);
			if (total === null) return;
			const promptDetails = usage.prompt_tokens_details as Record<string, unknown> | undefined;
			const completionDetails = usage.completion_tokens_details as Record<string, unknown> | undefined;
			latest = {
				total_usd: total,
				model: typeof row.model === "string" && row.model ? row.model : null,
				input_tokens: token(usage.prompt_tokens),
				output_tokens: token(usage.completion_tokens),
				cache_read_tokens: token(promptDetails?.cached_tokens),
				cache_write_tokens: token(promptDetails?.cache_write_tokens),
				reasoning_tokens: nonnegativeNumber(completionDetails?.reasoning_tokens),
			};
		} catch {
			// A malformed/non-usage SSE line is simply not an accounting record.
		}
	};
	try {
		while (true) {
			const { done, value } = await reader.read();
			buffer += decoder.decode(value, { stream: !done });
			let newline = buffer.indexOf("\n");
			while (newline >= 0) {
				inspect(buffer.slice(0, newline).replace(/\r$/, ""));
				buffer = buffer.slice(newline + 1);
				newline = buffer.indexOf("\n");
			}
			if (done) break;
		}
		inspect(buffer.replace(/\r$/, ""));
	} finally {
		reader.releaseLock();
	}
	return latest;
}

/**
 * Retrieve accounting only after the generation has settled. This deliberately
 * projects OpenRouter's response to a small redacted record: no generation id,
 * raw provider body, prompt, or credential is ever persisted.
 */
async function providerCost(
	responseId: string | undefined,
	turn: number,
	model: string,
	usage: { input: number; output: number; cacheRead: number; cacheWrite: number; reasoning?: number },
	apiKey: string,
): Promise<ProviderCostTurn> {
	const unavailable = (): ProviderCostTurn => ({
		turn,
		source: "unavailable",
		total_usd: null,
		upstream_inference_usd: null,
		model,
		provider: null,
		input_tokens: token(usage.input),
		output_tokens: token(usage.output),
		cache_read_tokens: token(usage.cacheRead),
		cache_write_tokens: token(usage.cacheWrite),
		reasoning_tokens: nonnegativeNumber(usage.reasoning),
	});
	if (!responseId) return unavailable();
	for (let attempt = 0; attempt < 3; attempt += 1) {
		try {
			const response = await fetch(
				`https://openrouter.ai/api/v1/generation?id=${encodeURIComponent(responseId)}`,
				{
					headers: { Authorization: `Bearer ${apiKey}` },
					signal: AbortSignal.timeout(15_000),
				},
			);
			if (response.ok) {
				const body: unknown = await response.json();
				const data = body && typeof body === "object" ? (body as { data?: unknown }).data : undefined;
				if (data && typeof data === "object") {
					const row = data as Record<string, unknown>;
					const total = nonnegativeNumber(row.total_cost);
					if (total !== null) {
						return {
							turn,
							source: "openrouter_generation",
							total_usd: total,
							upstream_inference_usd: nonnegativeNumber(row.upstream_inference_cost),
							model: typeof row.model === "string" && row.model ? row.model : model,
							provider: typeof row.provider_name === "string" && row.provider_name ? row.provider_name : null,
							input_tokens: token(row.tokens_prompt) || token(usage.input),
							output_tokens: token(row.tokens_completion) || token(usage.output),
							cache_read_tokens: token(row.tokens_cached),
							cache_write_tokens: token(row.tokens_cache_write),
							reasoning_tokens: nonnegativeNumber(row.tokens_reasoning) ?? nonnegativeNumber(usage.reasoning),
						};
					}
				}
			}
		} catch {
			// The projected unavailable record below makes failures visible without
			// retaining an arbitrary provider error payload in an evaluation artifact.
		}
		if (attempt < 2) await new Promise((resolve) => setTimeout(resolve, 150 * (attempt + 1)));
	}
	return unavailable();
}

function streamedProviderCost(
	usage: StreamUsageCost,
	turn: number,
	fallbackModel: string,
): ProviderCostTurn {
	return {
		turn,
		source: "openrouter_stream_usage",
		total_usd: usage.total_usd,
		upstream_inference_usd: null,
		model: usage.model ?? fallbackModel,
		provider: null,
		input_tokens: usage.input_tokens,
		output_tokens: usage.output_tokens,
		cache_read_tokens: usage.cache_read_tokens,
		cache_write_tokens: usage.cache_write_tokens,
		reasoning_tokens: usage.reasoning_tokens,
	};
}

function costReport(turns: ProviderCostTurn[]) {
	const reported = turns.filter((turn) => turn.total_usd !== null);
	return {
		schema_version: "pi-eval-cost/v1",
		currency: "USD",
		pricing: "provider_reported",
		reported_turn_count: reported.length,
		unavailable_turn_count: turns.length - reported.length,
		complete: reported.length === turns.length,
		reported_total_usd: reported.reduce((total, turn) => total + (turn.total_usd ?? 0), 0),
		reported_upstream_inference_usd: reported.reduce(
			(total, turn) => total + (turn.upstream_inference_usd ?? 0),
			0,
		),
		turns,
	};
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
	const observedStreamUsage: Promise<StreamUsageCost | null>[] = [];
	const accountingStream = ((selectedModel: unknown, context: unknown, options: any) => {
		const nextFetch = options?.fetch ?? globalThis.fetch;
		return openAICompletionsStream(selectedModel as never, context as never, {
			...options,
			fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
				const response = await nextFetch(input, init);
				try {
					observedStreamUsage.push(streamUsageCost(response.clone()));
				} catch {
					observedStreamUsage.push(Promise.resolve(null));
				}
				return response;
			},
		});
	}) as never;
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
		streamFn: accountingStream,
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
	const assistants = agent.state.messages.filter((message) => message.role === "assistant");
	const assistant = [...assistants].reverse()[0];
	if (!assistant || assistant.role !== "assistant") {
		throw new Error("pinned coding profile did not settle with an assistant response");
	}
	const finalText = assistant.content
		.filter((part) => part.type === "text")
		.map((part) => part.text)
		.join("");
	const streamedCosts = (await Promise.all(observedStreamUsage)).filter((usage): usage is StreamUsageCost => usage !== null);
	const costs = await Promise.all(
		assistants.map((message, index) => {
			const model = message.responseModel ?? message.model;
			const streamed = streamedCosts[index];
			return streamed
				? Promise.resolve(streamedProviderCost(streamed, index + 1, model))
				: providerCost(message.responseId, index + 1, model, message.usage, apiKey);
		}),
	);
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
		cost: costReport(costs),
		trace: eventTypes.map((type, seq) => ({ seq, type })),
	};
	await writeFile(args.resultJson, `${JSON.stringify(output)}\n`, "utf8");
}

main().catch((error: unknown) => {
	process.stderr.write(`pinned upstream coding-profile adapter: ${error instanceof Error ? error.message : String(error)}\n`);
	process.exitCode = 2;
});
