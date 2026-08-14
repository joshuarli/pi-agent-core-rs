/**
 * Run a closed V0 declarative fixture through the pinned Pi SDK in-process.
 *
 * The adapter has no provider, CLI, shell, or ambient configuration path. It
 * translates only deterministic text/tool-call stream chunks and host-tool
 * responses into the selected `Agent` API surface.
 */

import { isDeepStrictEqual } from "node:util";
import { readFile } from "node:fs/promises";
import { Agent } from "./source/packages/agent/src/agent.ts";
import { AssistantMessageEventStream } from "./source/packages/ai/src/utils/event-stream.ts";

type JsonObject = Record<string, unknown>;

function invalid(message: string): never {
	throw new Error(`invalid declarative fixture: ${message}`);
}

function object(value: unknown, path: string): JsonObject {
	if (!value || typeof value !== "object" || Array.isArray(value)) invalid(`${path} must be an object`);
	return value as JsonObject;
}

function string(value: unknown, path: string): string {
	if (typeof value !== "string") invalid(`${path} must be a string`);
	return value;
}

function boolean(value: unknown, path: string): boolean {
	if (typeof value !== "boolean") invalid(`${path} must be a boolean`);
	return value;
}

function executionMode(value: unknown, path: string): "parallel" | "sequential" {
	switch (string(value, path)) {
		case "parallel":
		case "sequential":
			return value;
		default:
			invalid(`${path} must be parallel or sequential`);
	}
}

function queueMode(value: unknown, path: string): "all" | "one-at-a-time" {
	switch (string(value, path)) {
		case "all":
		case "one-at-a-time":
			return value;
		default:
			invalid(`${path} must be all or one-at-a-time`);
	}
}

function array(value: unknown, path: string): unknown[] {
	if (!Array.isArray(value)) invalid(`${path} must be an array`);
	return value;
}

function usage(value: unknown, path: string): JsonObject {
	const source = object(value, path);
	for (const name of ["input", "output", "cache_read", "cache_write", "total_tokens"]) {
		if (typeof source[name] !== "number") invalid(`${path}.${name} must be a number`);
	}
	return source;
}

function makeUsage(source: JsonObject) {
	return {
		input: source.input,
		output: source.output,
		cacheRead: source.cache_read,
		cacheWrite: source.cache_write,
		totalTokens: source.total_tokens,
		cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
	};
}

function stopReason(value: unknown, path: string): "stop" | "toolUse" {
	switch (string(value, path)) {
		case "stop":
			return "stop";
		case "tool_call":
			return "toolUse";
		default:
			invalid(`${path} supports only stop or tool_call in the V0 adapter`);
	}
}

function canonicalStopReason(value: string): string {
	return value === "toolUse" ? "tool_call" : value;
}

function makeAssistant(content: unknown[], streamUsage: JsonObject, reason: "stop" | "toolUse") {
	return {
		role: "assistant" as const,
		content,
		api: "fixture",
		provider: "fixture",
		model: "deterministic",
		usage: makeUsage(streamUsage),
		stopReason: reason,
		timestamp: 0,
	};
}

function fixtureUserMessage(text: string) {
	return {
		role: "user" as const,
		content: [{ type: "text", text }],
		timestamp: 0,
	};
}

function normalizeContent(content: unknown[]): unknown[] {
	return content.map((part) => {
		const value = object(part, "message.content[*]");
		if (value.type === "toolCall") {
			return { type: "tool_call", id: value.id, name: value.name, arguments: value.arguments };
		}
		return value;
	});
}

function normalizeMessage(message: unknown): JsonObject {
	const value = object(message, "agent message");
	const role = string(value.role, "agent message.role") === "toolResult" ? "tool_result" : string(value.role, "agent message.role");
	const content = Array.isArray(value.content)
		? normalizeContent(value.content)
		: [{ type: "text", text: string(value.content, "agent message.content") }];
	return { role, content };
}

function makeTools(setupTools: unknown[], hostTools: unknown[]) {
	const hostByName = new Map<string, unknown[]>();
	for (const rawHostTool of hostTools) {
		const hostTool = object(rawHostTool, "host.tools[*]");
		const name = string(hostTool.name, "host.tools[*].name");
		if (hostByName.has(name)) invalid(`host.tools repeats ${JSON.stringify(name)}`);
		hostByName.set(name, array(hostTool.calls, "host.tools[*].calls"));
	}
	const names = new Set<string>();
	return setupTools.map((rawTool) => {
		const tool = object(rawTool, "setup.tools[*]");
		const name = string(tool.name, "setup.tools[*].name");
		if (names.has(name)) invalid(`setup.tools repeats ${JSON.stringify(name)}`);
		names.add(name);
		const description = string(tool.description, `setup.tools.${name}.description`);
		const parameters = object(tool.parameters, `setup.tools.${name}.parameters`);
		const toolExecutionMode =
			tool.execution_mode === undefined
				? ("parallel" as const)
				: executionMode(tool.execution_mode, `setup.tools.${name}.execution_mode`);
		const calls = hostByName.get(name) ?? [];
		return {
			name,
			label: name,
			description,
			parameters,
			executionMode: toolExecutionMode,
			async execute(_toolCallId: string, arguments_: unknown, _signal, onUpdate) {
				const index = calls.findIndex((rawCall) => {
					const call = object(rawCall, `host.tools.${name}.calls[*]`);
					return isDeepStrictEqual(call.arguments, arguments_);
				});
				if (index < 0) invalid(`host tool ${JSON.stringify(name)} has no matching response`);
				const call = object(calls.splice(index, 1)[0], `host.tools.${name}.calls[*]`);
				const yieldOnce =
					call.yield_once === undefined
						? false
						: boolean(call.yield_once, `host.tools.${name}.calls[*].yield_once`);
				const updates =
					call.updates === undefined
						? []
						: array(call.updates, `host.tools.${name}.calls[*].updates`).map((value, updateIndex) =>
							string(value, `host.tools.${name}.calls[*].updates[${updateIndex}]`),
						);
				const result = object(call.result, `host.tools.${name}.calls[*].result`);
				const content = array(result.content, `host.tools.${name}.calls[*].result.content`);
				if (content.length !== 1 || object(content[0], "host tool result content").type !== "text") {
					invalid("V0 fixture adapter supports exactly one text tool-result content part");
				}
				if (boolean(result.is_error, `host.tools.${name}.calls[*].result.is_error`)) {
					throw new Error(string(object(content[0], "host tool result content").text, "host tool result content.text"));
				}
				for (const update of updates) onUpdate?.({ content: [{ type: "text", text: update }], details: {} });
				if (yieldOnce) await new Promise<void>((resolve) => queueMicrotask(resolve));
				return { content, details: {} };
			},
		};
	});
}

function makeBeforeToolCall(host: JsonObject) {
	if (host.before_tool_call === undefined) return undefined;
	const rule = object(host.before_tool_call, "host.before_tool_call");
	const toolName = string(rule.tool_name, "host.before_tool_call.tool_name");
	const reason = string(rule.reason, "host.before_tool_call.reason");
	return async (context: { toolCall: { name: string } }) =>
		context.toolCall.name === toolName ? { block: true, reason } : undefined;
}

function makeAfterToolCall(host: JsonObject) {
	if (host.after_tool_call === undefined) return undefined;
	const rule = object(host.after_tool_call, "host.after_tool_call");
	const toolName = string(rule.tool_name, "host.after_tool_call.tool_name");
	const content = string(rule.content, "host.after_tool_call.content");
	const isError = boolean(rule.is_error, "host.after_tool_call.is_error");
	return async (context: { toolCall: { name: string } }) =>
		context.toolCall.name === toolName
			? { content: [{ type: "text", text: content }], isError }
			: undefined;
}

function makeStreamFunction(modelScript: unknown[]) {
	let callIndex = 0;
	return () => {
		const rawTurn = modelScript[callIndex++];
		if (!rawTurn) invalid("agent made more model requests than model_script provides");
		const turn = object(rawTurn, "model_script[*]");
		const chunks = array(turn.chunks, "model_script[*].chunks");
		if (chunks.length === 0) invalid("model_script[*].chunks must not be empty");
		const rawDone = object(chunks.at(-1), "model_script[*].chunks[-1]");
		if (rawDone.kind !== "done") invalid("each model_script turn must end with done");
		const finalUsage = usage(rawDone.usage, "model_script[*].chunks[-1].usage");
		const finalReason = stopReason(rawDone.stop_reason, "model_script[*].chunks[-1].stop_reason");
		const content: unknown[] = [];
		let text = "";
		let hasText = false;
		for (const [index, rawChunk] of chunks.slice(0, -1).entries()) {
			const chunk = object(rawChunk, `model_script[*].chunks[${index}]`);
			switch (chunk.kind) {
				case "text_delta":
					text += string(chunk.text, `model_script[*].chunks[${index}].text`);
					hasText = true;
					break;
				case "tool_call":
					content.push({
						type: "toolCall",
						id: string(chunk.id, `model_script[*].chunks[${index}].id`),
						name: string(chunk.name, `model_script[*].chunks[${index}].name`),
						arguments: object(chunk.arguments, `model_script[*].chunks[${index}].arguments`),
					});
					break;
				default:
					invalid(`unsupported model-script chunk kind ${JSON.stringify(chunk.kind)}`);
			}
		}
		if (hasText) content.unshift({ type: "text", text });
		const stream = new AssistantMessageEventStream();
		queueMicrotask(() => {
			if (hasText) {
				stream.push({ type: "start", partial: makeAssistant([{ type: "text", text: "" }], finalUsage, finalReason) });
				let partial = "";
				for (const rawChunk of chunks.slice(0, -1)) {
					const chunk = object(rawChunk, "text chunk");
					if (chunk.kind !== "text_delta") continue;
					const delta = string(chunk.text, "text chunk.text");
					partial += delta;
					stream.push({
						type: "text_delta",
						contentIndex: 0,
						delta,
						partial: makeAssistant([{ type: "text", text: partial }], finalUsage, finalReason),
					});
				}
			}
			stream.push({ type: "done", reason: finalReason, message: makeAssistant(content, finalUsage, finalReason) });
		});
		return stream;
	};
}

async function main(): Promise<void> {
	const fixturePath = process.argv[2];
	if (!fixturePath || process.argv.length !== 3) invalid("expected exactly one declarative fixture path");
	const fixture = object(JSON.parse(await readFile(fixturePath, "utf8")), "fixture");
	if (fixture.format_version !== 1 || fixture.kind !== "declarative_parity_fixture") {
		invalid("requires format_version 1 declarative_parity_fixture");
	}
	const setup = object(fixture.setup, "setup");
	const model = object(setup.model, "setup.model");
	const setupTools = array(setup.tools, "setup.tools");
	const host = object(fixture.host, "host");
	const tools = makeTools(setupTools, array(host.tools, "host.tools"));
	const beforeToolCall = makeBeforeToolCall(host);
	const afterToolCall = makeAfterToolCall(host);
	const steeringMode =
		setup.steering_mode === undefined ? undefined : queueMode(setup.steering_mode, "setup.steering_mode");
	const followUpMode =
		setup.follow_up_mode === undefined ? undefined : queueMode(setup.follow_up_mode, "setup.follow_up_mode");
	const actions = array(fixture.actions, "actions");
	if (actions.length === 0) invalid("V0 runner requires at least one action");
	const fixtureActions = actions.map((rawAction, index) => {
		const fixtureAction = object(rawAction, `actions[${index}]`);
		const kind = string(fixtureAction.kind, `actions[${index}].kind`);
		switch (kind) {
			case "steer":
			case "follow_up":
			case "prompt":
				return { kind, text: string(fixtureAction.text, `actions[${index}].text`) };
			case "continue":
				return { kind };
			default:
				invalid(`V0 runner does not support action ${JSON.stringify(kind)}`);
		}
	});
	if (!fixtureActions.some((action) => action.kind === "prompt" || action.kind === "continue")) {
		invalid("V0 runner requires an action that starts a run");
	}
	const modelScript = array(fixture.model_script, "model_script");
	const streamFn = makeStreamFunction(modelScript);

	const agent = new Agent({
		streamFn,
		beforeToolCall,
		afterToolCall,
		steeringMode,
		followUpMode,
		initialState: {
			systemPrompt: string(setup.system_prompt, "setup.system_prompt"),
			model: { api: "fixture", provider: string(model.provider, "setup.model.provider"), id: string(model.id, "setup.model.id") },
			thinkingLevel: string(setup.thinking_level, "setup.thinking_level") as never,
			tools,
		},
	});
	const events: JsonObject[] = [];
	let turn = 0;
	agent.subscribe((event) => {
		switch (event.type) {
			case "agent_start":
			case "agent_end":
				events.push({ type: event.type, data: {} });
				break;
			case "turn_start":
				events.push({ type: event.type, data: { turn: turn++ } });
				break;
			case "turn_end":
				events.push({ type: event.type, data: { stop_reason: canonicalStopReason(event.message.stopReason) } });
				break;
			case "message_start":
			case "message_end":
				events.push({ type: event.type, data: { role: event.message.role === "toolResult" ? "tool_result" : event.message.role } });
				break;
			case "message_update":
				events.push({
					type: event.type,
					data: {
						role: event.message.role === "toolResult" ? "tool_result" : event.message.role,
						delta: event.assistantMessageEvent.type === "text_delta" ? event.assistantMessageEvent.delta : undefined,
					},
				});
				break;
			case "tool_execution_start":
				events.push({ type: event.type, data: { tool_call_id: event.toolCallId, tool_name: event.toolName } });
				break;
			case "tool_execution_update":
				const partialContent = array(event.partialResult.content, "tool update content");
				if (partialContent.length !== 1 || object(partialContent[0], "tool update content part").type !== "text") {
					invalid("V0 fixture adapter supports exactly one text tool update content part");
				}
				events.push({
					type: event.type,
					data: {
						tool_call_id: event.toolCallId,
						tool_name: event.toolName,
						content: string(object(partialContent[0], "tool update content part").text, "tool update content text"),
					},
				});
				break;
			case "tool_execution_end":
				events.push({
					type: event.type,
					data: { tool_call_id: event.toolCallId, tool_name: event.toolName, is_error: event.isError },
				});
				break;
			default:
				invalid(`V0 runner observed unsupported ${event.type} event`);
		}
	});
	for (const fixtureAction of fixtureActions) {
		switch (fixtureAction.kind) {
			case "steer":
				agent.steer(fixtureUserMessage(fixtureAction.text));
				break;
			case "follow_up":
				agent.followUp(fixtureUserMessage(fixtureAction.text));
				break;
			case "prompt":
				await agent.prompt(fixtureAction.text);
				break;
			case "continue":
				await agent.continue();
				break;
		}
	}

	if (modelScript.length !== turn) invalid("model_script contains unused turns");
	events.push({ type: "agent_settled", data: { outcome: "completed" } });
	const lastMessage = agent.state.messages.at(-1);
	if (!lastMessage || lastMessage.role !== "assistant") invalid("agent did not settle with an assistant message");
	const result = {
		format_version: 1,
		kind: "canonical_parity_result",
		fixture_id: string(fixture.id, "id"),
		outcome: "completed",
		settled: true,
		state: {
			system_prompt: agent.state.systemPrompt,
			model: { provider: agent.state.model.provider, id: agent.state.model.id },
			thinking_level: agent.state.thinkingLevel,
			tool_names: agent.state.tools.map((tool) => tool.name),
			pending_tool_calls: [...agent.state.pendingToolCalls],
		},
		events: events.map((event, seq) => ({ seq, ...event })),
		messages: agent.state.messages.map(normalizeMessage),
		last_response: { api: lastMessage.api, stop_reason: canonicalStopReason(lastMessage.stopReason) },
		usage: {
			input: lastMessage.usage.input,
			output: lastMessage.usage.output,
			cache_read: lastMessage.usage.cacheRead,
			cache_write: lastMessage.usage.cacheWrite,
			total_tokens: lastMessage.usage.totalTokens,
		},
		error: null,
	};
	process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
}

main().catch((error: unknown) => {
	process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
	process.exitCode = 2;
});
