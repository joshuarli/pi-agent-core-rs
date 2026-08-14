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

function thinkingLevel(value: unknown, path: string): "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" {
	switch (string(value, path)) {
		case "off":
		case "minimal":
		case "low":
		case "medium":
		case "high":
		case "xhigh":
		case "max":
			return value;
		default:
			invalid(`${path} has an unsupported thinking level`);
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

function stopReason(value: unknown, path: string): "stop" | "toolUse" | "length" {
	switch (string(value, path)) {
		case "stop":
			return "stop";
		case "tool_call":
			return "toolUse";
		case "length":
			return "length";
		default:
			invalid(`${path} supports only stop, tool_call, or length in the V0 adapter`);
	}
}

function errorStopReason(value: unknown, path: string): "error" | "aborted" {
	switch (string(value, path)) {
		case "error":
		case "aborted":
			return value;
		default:
			invalid(`${path} must be error or aborted`);
	}
}

function canonicalStopReason(value: string): string {
	return value === "toolUse" ? "tool_call" : value;
}

function makeAssistant(
	content: unknown[],
	streamUsage: JsonObject,
	reason: "stop" | "toolUse" | "length" | "error" | "aborted",
	errorMessage?: string,
) {
	return {
		role: "assistant" as const,
		content,
		api: "fixture",
		provider: "fixture",
		model: "deterministic",
		usage: makeUsage(streamUsage),
		stopReason: reason,
		errorMessage,
		timestamp: 0,
	};
}

function cancelAfterTextDelta(value: unknown, path: string): boolean {
	if (value === undefined) return false;
	if (value !== "text_delta") invalid(`${path} must be text_delta`);
	return true;
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

type ActiveQueueArrival = { kind: "steer" | "follow_up"; text: string };

type ObserverSettlementGate = {
	reached: Promise<void>;
	release: () => void;
	listener: (event: { type: string }) => Promise<void>;
};

function observerSettlementGate(host: JsonObject): ObserverSettlementGate | undefined {
	if (host.observer === undefined) return undefined;
	const observer = object(host.observer, "host.observer");
	const holdAgentEnd = boolean(observer.hold_agent_end, "host.observer.hold_agent_end");
	if (!holdAgentEnd) invalid("host.observer.hold_agent_end must be true in the V0 fixture adapter");
	let resolveReached!: () => void;
	let resolveRelease!: () => void;
	const reached = new Promise<void>((resolve) => {
		resolveReached = resolve;
	});
	const release = new Promise<void>((resolve) => {
		resolveRelease = resolve;
	});
	return {
		reached,
		release: resolveRelease,
		listener: async (event) => {
			if (event.type === "agent_end") {
				resolveReached();
				await release;
			}
		},
	};
}

function activeQueueArrival(value: unknown, path: string): ActiveQueueArrival {
	const arrival = object(value, path);
	const kind = string(arrival.kind, `${path}.kind`);
	if (kind !== "steer" && kind !== "follow_up") {
		invalid(`${path}.kind must be steer or follow_up`);
	}
	return { kind, text: string(arrival.text, `${path}.text`) };
}

function makeTools(
	setupTools: unknown[],
	hostTools: unknown[],
	cancelCurrentRun: () => void,
	enqueueActiveRun: (arrival: ActiveQueueArrival) => void,
) {
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
				const cancelAfterUpdate =
					call.cancel_after_update === undefined
						? false
						: boolean(call.cancel_after_update, `host.tools.${name}.calls[*].cancel_after_update`);
				if (cancelAfterUpdate && updates.length === 0) {
					invalid(`host.tools.${name}.calls[*].cancel_after_update requires at least one update`);
				}
				const enqueueDuringExecution =
					call.enqueue_during_execution === undefined
						? undefined
						: activeQueueArrival(
								call.enqueue_during_execution,
								`host.tools.${name}.calls[*].enqueue_during_execution`,
							);
				if (enqueueDuringExecution && !yieldOnce) {
					invalid(`host.tools.${name}.calls[*].enqueue_during_execution requires yield_once`);
				}
				const result = object(call.result, `host.tools.${name}.calls[*].result`);
				const terminate =
					result.terminate === undefined
						? undefined
						: boolean(result.terminate, `host.tools.${name}.calls[*].result.terminate`);
				const content = array(result.content, `host.tools.${name}.calls[*].result.content`);
				if (content.length !== 1 || object(content[0], "host tool result content").type !== "text") {
					invalid("V0 fixture adapter supports exactly one text tool-result content part");
				}
				if (boolean(result.is_error, `host.tools.${name}.calls[*].result.is_error`)) {
					throw new Error(string(object(content[0], "host tool result content").text, "host tool result content.text"));
				}
				for (const update of updates) {
					onUpdate?.({ content: [{ type: "text", text: update }], details: {} });
					if (cancelAfterUpdate) cancelCurrentRun();
				}
				if (yieldOnce) await new Promise<void>((resolve) => queueMicrotask(resolve));
				if (enqueueDuringExecution) enqueueActiveRun(enqueueDuringExecution);
				return { content, details: {}, ...(terminate === undefined ? {} : { terminate }) };
			},
		};
	});
}

function makeBeforeToolCall(host: JsonObject, cancelCurrentRun: () => void) {
	if (host.before_tool_call === undefined) return undefined;
	const rule = object(host.before_tool_call, "host.before_tool_call");
	const toolName = string(rule.tool_name, "host.before_tool_call.tool_name");
	const reason = string(rule.reason, "host.before_tool_call.reason");
	const terminate =
		rule.terminate === undefined ? false : boolean(rule.terminate, "host.before_tool_call.terminate");
	const yieldOnce = rule.yield_once === undefined ? false : boolean(rule.yield_once, "host.before_tool_call.yield_once");
	const cancelAfterYield =
		rule.cancel_after_yield === undefined
			? false
			: boolean(rule.cancel_after_yield, "host.before_tool_call.cancel_after_yield");
	if (cancelAfterYield && !yieldOnce) {
		invalid("host.before_tool_call.cancel_after_yield requires yield_once");
	}
	return async (context: { toolCall: { name: string } }) => {
		if (context.toolCall.name !== toolName) return undefined;
		if (yieldOnce) await new Promise<void>((resolve) => queueMicrotask(resolve));
		if (cancelAfterYield) {
			cancelCurrentRun();
			return undefined;
		}
		return { block: true, reason, ...(terminate ? { terminate: true } : {}) };
	};
}

function makeAfterToolCall(host: JsonObject) {
	if (host.after_tool_call === undefined) return undefined;
	const rule = object(host.after_tool_call, "host.after_tool_call");
	const toolName = string(rule.tool_name, "host.after_tool_call.tool_name");
	const content = string(rule.content, "host.after_tool_call.content");
	const isError = boolean(rule.is_error, "host.after_tool_call.is_error");
	const terminate =
		rule.terminate === undefined ? undefined : boolean(rule.terminate, "host.after_tool_call.terminate");
	return async (context: { toolCall: { name: string } }) =>
		context.toolCall.name === toolName
			? { content: [{ type: "text", text: content }], isError, ...(terminate === undefined ? {} : { terminate }) }
			: undefined;
}

function makeShouldStopAfterTurn(host: JsonObject) {
	if (host.should_stop_after_turn === undefined) return undefined;
	const stop = boolean(host.should_stop_after_turn, "host.should_stop_after_turn");
	return async () => stop;
}

type ContextHooks = {
	hostMessages: string[];
	transformAppend: string;
	convertPrefix: string;
	nextHostMessages: string[];
	nextProvider: string;
	nextModel: string;
	nextThinkingLevel: "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max";
};

function makeContextHooks(value: unknown): ContextHooks | undefined {
	if (value === undefined) return undefined;
	const source = object(value, "setup.context_hooks");
	const hostMessages = array(source.host_messages, "setup.context_hooks.host_messages").map((item, index) =>
		string(item, `setup.context_hooks.host_messages[${index}]`),
	);
	const next = object(source.prepare_next_turn, "setup.context_hooks.prepare_next_turn");
	const nextModel = object(next.model, "setup.context_hooks.prepare_next_turn.model");
	return {
		hostMessages,
		transformAppend: string(source.transform_append_host_message, "setup.context_hooks.transform_append_host_message"),
		convertPrefix: string(source.convert_prefix, "setup.context_hooks.convert_prefix"),
		nextHostMessages: array(next.host_messages, "setup.context_hooks.prepare_next_turn.host_messages").map((item, index) =>
			string(item, `setup.context_hooks.prepare_next_turn.host_messages[${index}]`),
		),
		nextProvider: string(nextModel.provider, "setup.context_hooks.prepare_next_turn.model.provider"),
		nextModel: string(nextModel.id, "setup.context_hooks.prepare_next_turn.model.id"),
		nextThinkingLevel: thinkingLevel(next.thinking_level, "setup.context_hooks.prepare_next_turn.thinking_level"),
	};
}

function fixtureModel(provider: string, id: string) {
	return {
		id,
		name: id,
		api: "fixture" as const,
		provider,
		baseUrl: "",
		reasoning: false,
		input: ["text" as const],
		cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
		contextWindow: 0,
		maxTokens: 0,
	};
}

function fixtureHostMessage(value: string) {
	return fixtureUserMessage(`__fixture_host__:${value}`);
}

function hostMessageValue(message: unknown): string | undefined {
	const value = object(message, "context message");
	if (value.role !== "user" || !Array.isArray(value.content)) return undefined;
	const content = value.content[0];
	if (!content || typeof content !== "object" || Array.isArray(content)) return undefined;
	const text = (content as JsonObject).text;
	if (typeof text !== "string" || !text.startsWith("__fixture_host__:")) return undefined;
	return text.slice("__fixture_host__:".length);
}

function makeContextHookOptions(contextHooks: ContextHooks, initialModel: ReturnType<typeof fixtureModel>) {
	return {
		transformContext: async (messages: unknown[]) => {
			const hasHostContext = messages.some((message) => hostMessageValue(message) !== undefined);
			return [
				...messages,
				...(hasHostContext ? [] : contextHooks.hostMessages.map(fixtureHostMessage)),
				fixtureHostMessage(contextHooks.transformAppend),
			];
		},
		convertToLlm: (messages: unknown[]) => {
			const values = messages.map(hostMessageValue).filter((value): value is string => value !== undefined);
			return [fixtureUserMessage(`${contextHooks.convertPrefix}${values.join("|")}`)];
		},
		prepareNextTurnWithContext: async (turn: { context: { systemPrompt: string; messages: unknown[]; tools: unknown[] } }) => ({
			context: {
				...turn.context,
				messages: contextHooks.nextHostMessages.map(fixtureHostMessage),
			},
			model: { ...initialModel, provider: contextHooks.nextProvider, id: contextHooks.nextModel },
			thinkingLevel: contextHooks.nextThinkingLevel,
		}),
	};
}

function makeStreamFunction(
	modelScript: unknown[],
	abortCurrentRun: () => void,
	onRequest: () => void,
	requestTrace: JsonObject[],
) {
	let callIndex = 0;
	return (model: { provider: string; id: string }, context: { messages: unknown[] }, options?: { signal?: AbortSignal; reasoning?: string }) => {
		onRequest();
		const requestContext = context.messages
			.map((message) => {
				const value = object(message, "request context message");
				const content = array(value.content, "request context message.content");
				return content
					.map((part) => {
						const text = object(part, "request context content").text;
						return typeof text === "string" ? text : "";
					})
					.join("");
			})
			.join("|");
		requestTrace.push({
			context: requestContext,
			model: { provider: model.provider, id: model.id },
			thinking_level: options?.reasoning ?? "off",
		});
		const rawTurn = modelScript[callIndex++];
		if (!rawTurn) invalid("agent made more model requests than model_script provides");
		const turn = object(rawTurn, "model_script[*]");
		const cancelAfterDelta = cancelAfterTextDelta(
			turn.cancel_after,
			`model_script[${callIndex - 1}].cancel_after`,
		);
		const chunks = array(turn.chunks, "model_script[*].chunks");
		if (chunks.length === 0) invalid("model_script[*].chunks must not be empty");
		const terminal = object(chunks.at(-1), "model_script[*].chunks[-1]");
		if (terminal.kind !== "done" && terminal.kind !== "error") {
			invalid("each model_script turn must end with done or error");
		}
		const finalUsage = usage(terminal.usage, "model_script[*].chunks[-1].usage");
		const finalReason =
			terminal.kind === "done"
				? stopReason(terminal.stop_reason, "model_script[*].chunks[-1].stop_reason")
				: errorStopReason(terminal.reason, "model_script[*].chunks[-1].reason");
		const errorMessage =
			terminal.kind === "error"
				? string(terminal.message, "model_script[*].chunks[-1].message")
				: undefined;
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
		if (cancelAfterDelta && !hasText) {
			invalid("model_script[*].cancel_after text_delta requires a text_delta chunk");
		}
		queueMicrotask(() => {
			if (options?.signal?.aborted) {
				stream.push({
					type: "error",
					reason: "aborted",
					error: makeAssistant([], finalUsage, "aborted", "Operation aborted"),
				});
				return;
			}
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
					if (cancelAfterDelta) {
						abortCurrentRun();
						stream.push({
							type: "error",
							reason: "aborted",
							error: makeAssistant(
								[{ type: "text", text: partial }],
								finalUsage,
								"aborted",
								"Operation aborted",
							),
						});
						return;
					}
				}
			}
			const message = makeAssistant(content, finalUsage, finalReason, errorMessage);
			if (terminal.kind === "error") {
				stream.push({ type: "error", reason: finalReason, error: message });
			} else {
				stream.push({ type: "done", reason: finalReason, message });
			}
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
	const observerGate = observerSettlementGate(host);
	const contextHooks = makeContextHooks(setup.context_hooks);
	let abortCurrentRun = () => {};
	let cancellationRequested = false;
	const requestCancellation = () => {
		cancellationRequested = true;
		abortCurrentRun();
	};
	let enqueueActiveRun = (_arrival: ActiveQueueArrival) => invalid("tool attempted to queue before the agent was ready");
	const tools = makeTools(setupTools, array(host.tools, "host.tools"), requestCancellation, (arrival) =>
		enqueueActiveRun(arrival),
	);
	const beforeToolCall = makeBeforeToolCall(host, requestCancellation);
	const afterToolCall = makeAfterToolCall(host);
	const shouldStopAfterTurn = makeShouldStopAfterTurn(host);
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
	if (observerGate && fixtureActions.filter((action) => action.kind === "prompt" || action.kind === "continue").length !== 1) {
		invalid("host.observer.hold_agent_end requires exactly one run-starting action");
	}
	const modelScript = array(fixture.model_script, "model_script");
	let streamRequests = 0;
	const requestTrace: JsonObject[] = [];
	const initialModel = fixtureModel(string(model.provider, "setup.model.provider"), string(model.id, "setup.model.id"));
	const streamFn = makeStreamFunction(modelScript, requestCancellation, () => {
		streamRequests += 1;
	}, requestTrace);

	const agent = new Agent({
		streamFn,
		beforeToolCall,
		afterToolCall,
		shouldStopAfterTurn,
		steeringMode,
		followUpMode,
		...(contextHooks === undefined ? {} : makeContextHookOptions(contextHooks, initialModel)),
		initialState: {
			systemPrompt: string(setup.system_prompt, "setup.system_prompt"),
			model: initialModel,
			thinkingLevel: string(setup.thinking_level, "setup.thinking_level") as never,
			tools,
		},
	});
	abortCurrentRun = () => agent.abort();
	enqueueActiveRun = (arrival) => {
		const message = fixtureUserMessage(arrival.text);
		if (arrival.kind === "steer") {
			agent.steer(message);
		} else {
			agent.followUp(message);
		}
	};
	const events: JsonObject[] = [];
	let turn = 0;
	let outcome = "completed";
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
	if (observerGate) agent.subscribe(observerGate.listener);
	let observerActiveBeforeRelease: boolean | undefined;
	for (const fixtureAction of fixtureActions) {
		switch (fixtureAction.kind) {
			case "steer":
				agent.steer(fixtureUserMessage(fixtureAction.text));
				break;
			case "follow_up":
				agent.followUp(fixtureUserMessage(fixtureAction.text));
				break;
			case "prompt":
				if (observerGate) {
					const run = agent.prompt(fixtureAction.text);
					await observerGate.reached;
					let idle = false;
					const idleWait = agent.waitForIdle().then(() => {
						idle = true;
					});
					await Promise.resolve();
					observerActiveBeforeRelease = !idle;
					if (!observerActiveBeforeRelease) invalid("agent settled before its awaited agent_end listener released");
					observerGate.release();
					await run;
					await idleWait;
				} else {
					await agent.prompt(fixtureAction.text);
				}
				outcome = "completed";
				break;
			case "continue":
				if (observerGate) {
					const run = agent.continue();
					await observerGate.reached;
					let idle = false;
					const idleWait = agent.waitForIdle().then(() => {
						idle = true;
					});
					await Promise.resolve();
					observerActiveBeforeRelease = !idle;
					if (!observerActiveBeforeRelease) invalid("agent settled before its awaited agent_end listener released");
					observerGate.release();
					await run;
					await idleWait;
				} else {
					await agent.continue();
				}
				outcome = "completed";
				break;
		}
	}

	if (modelScript.length !== streamRequests) {
		invalid(`model_script has ${modelScript.length} turn(s), but the agent requested ${streamRequests}`);
	}
	// A checkpoint can abort one invocation and a later prompt can reuse the
	// same Agent. Only the terminal turn determines this aggregate projection;
	// an earlier successful tool turn must not mask a later abort.
	const terminalTurn = [...events].reverse().find((event) => event.type === "turn_end");
	if (
		cancellationRequested &&
		terminalTurn &&
		object(terminalTurn.data, "terminal turn data").stop_reason === "aborted"
	) {
		outcome = "cancelled";
	}
	events.push({ type: "agent_settled", data: { outcome } });
	const lastAssistant = [...agent.state.messages].reverse().find((message) => message.role === "assistant");
	if (!lastAssistant) invalid("agent did not emit an assistant message");
	const result: JsonObject = {
		format_version: 1,
		kind: "canonical_parity_result",
		fixture_id: string(fixture.id, "id"),
		outcome,
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
		last_response: { api: lastAssistant.api, stop_reason: canonicalStopReason(lastAssistant.stopReason) },
		usage: {
			input: lastAssistant.usage.input,
			output: lastAssistant.usage.output,
			cache_read: lastAssistant.usage.cacheRead,
			cache_write: lastAssistant.usage.cacheWrite,
			total_tokens: lastAssistant.usage.totalTokens,
		},
		error: null,
	};
	if (contextHooks !== undefined) result.request_trace = requestTrace;
	if (observerGate) {
		result.observer_settlement = {
			agent_end_observed: true,
			active_before_release: observerActiveBeforeRelease === true,
			idle_after_release: true,
		};
	}
	process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
}

main().catch((error: unknown) => {
	process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
	process.exitCode = 2;
});
