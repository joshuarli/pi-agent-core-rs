/**
 * Capture one redacted OpenRouter terminal response through the pinned Pi SDK source.
 *
 * Run only from `parity/upstream/source` with the caller's secret injector:
 *
 *   vault OPENROUTER_API_KEY -- env OPENROUTER_MODEL=poolside/laguna-xs-2.1 \
 *     ./node_modules/.bin/tsx ../record-openrouter.mts
 *
 * This imports the pinned Agent and OpenAI-compatible Pi provider adapter directly. It never
 * invokes a host-installed `pi` executable, writes no files, and prints no credential.
 */

import { Agent } from "./source/packages/agent/src/agent.ts";
import { stream as openAICompletionsStream } from "./source/packages/ai/src/api/openai-completions.ts";

// The model is explicit command input rather than a property of a host Pi installation. Keep
// the old unavailable-model default so re-running the documented command without a model does
// not silently overwrite that error-path fixture.
const modelId = process.env.OPENROUTER_MODEL ?? "inclusionai/ling-3.0-tiny:free";
const model = {
	id: modelId,
	name: modelId,
	api: "openai-completions",
	provider: "openrouter",
	baseUrl: "https://openrouter.ai/api/v1",
	reasoning: false,
	input: ["text"],
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
	contextWindow: 32768,
	maxTokens: 128,
} as const;

const systemPrompt = "Reply concisely and do not call tools.";
const userText = "Respond with exactly: fixture capture succeeded.";

function normalizeEvent(event: Record<string, unknown>) {
	switch (event.type) {
		case "agent_start":
		case "agent_end":
		case "turn_start":
			return { type: event.type };
		case "turn_end": {
			const message = event.message as { stopReason: string };
			return { type: event.type, stop_reason: message.stopReason };
		}
		case "message_start":
		case "message_end": {
			const message = event.message as { role: string };
			return { type: event.type, role: message.role };
		}
		case "message_update":
			return { type: event.type };
		default:
			return { type: String(event.type) };
	}
}

async function main(): Promise<void> {
	const apiKey = process.env.OPENROUTER_API_KEY;
	if (!apiKey) throw new Error("OPENROUTER_API_KEY was not supplied by the secret injector");
	const events: unknown[] = [];
	const agent = new Agent({
		streamFn: openAICompletionsStream as never,
		getApiKey: (provider) => (provider === "openrouter" ? apiKey : undefined),
		initialState: {
			systemPrompt,
			model: model as never,
			thinkingLevel: "off",
			tools: [],
		},
	});
	agent.subscribe((event) => {
		events.push(normalizeEvent(event as unknown as Record<string, unknown>));
	});
	await agent.prompt(userText);
	const assistant = agent.state.messages.at(-1);
	if (!assistant || assistant.role !== "assistant") throw new Error("pinned Agent did not settle with assistant output");
	const result = {
		format_version: 1,
		kind: "recorded_pi_sdk_terminal_response",
		capture: {
			pi_agent_core_version: "0.84.1",
			pi_commit: "9d2ec7ffabe927bfad2214c1cee25b6632a78dcf",
			captured_on: new Date().toISOString().slice(0, 10),
			capture_runner: "pinned-source-agent-sdk",
			provider: "openrouter",
			model: model.id,
			redaction: {
				removed: ["OPENROUTER_API_KEY", "authorization headers", "session id", "timestamps"],
			},
		},
		request: { system_prompt: systemPrompt, user_text: userText },
		events,
		assistant: {
			api: assistant.api,
			provider: assistant.provider,
			model: assistant.model,
			stop_reason: assistant.stopReason,
			error_message: assistant.errorMessage ?? null,
			content: assistant.content,
			usage: assistant.usage,
		},
	};
	process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
}

main().catch((error: unknown) => {
	process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
	process.exitCode = 2;
});
