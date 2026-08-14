/**
 * Verify the checked-in default profile against the pinned Pi SDK source.
 *
 * Run from parity/upstream/source after npm ci --ignore-scripts:
 *
 *   ./node_modules/.bin/tsx ../../profile/verify-profile.mts
 *
 * This imports the SDK factories directly. It never starts the pi executable, creates an Agent,
 * contacts a provider, or invokes a standard tool operation. The behavior manifest is checked for
 * complete case coverage here; operation execution belongs to the explicit profile adapter tests.
 */

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { getDocsPath, getExamplesPath, getReadmePath } from "../upstream/source/packages/coding-agent/src/config.ts";
import { buildSystemPrompt } from "../upstream/source/packages/coding-agent/src/core/system-prompt.ts";
import {
	createAllToolDefinitions,
	createCodingToolDefinitions,
	type ToolDef,
} from "../upstream/source/packages/coding-agent/src/core/tools/index.ts";
import { capture as captureProfileBehavior, grepBlocker } from "../upstream/profile-behavior-runner.mts";

const scriptDirectory = resolve(fileURLToPath(new URL(".", import.meta.url)));
const repositoryRoot = resolve(scriptDirectory, "../..");
const profilePath = resolve(scriptDirectory, "default-profile.json");
const sourceManifestPath = resolve(scriptDirectory, "source-manifest.json");
const behaviorManifestPath = resolve(scriptDirectory, "behavior-manifest.json");
const workspaceRoot = "/fixture/workspace";
const documentationPaths = {
	readme: "/fixture/pi/README.md",
	docs: "/fixture/pi/docs",
	examples: "/fixture/pi/examples",
} as const;

type JsonObject = Record<string, unknown>;

function readJson(path: string): JsonObject {
	return JSON.parse(readFileSync(path, "utf8")) as JsonObject;
}

function sha256(value: string | Buffer): string {
	return createHash("sha256").update(value).digest("hex");
}

function assertEqual(actual: unknown, expected: unknown, label: string): void {
	const actualJson = JSON.stringify(actual);
	const expectedJson = JSON.stringify(expected);
	if (actualJson !== expectedJson) {
		throw new Error(`${label} mismatch\nexpected: ${expectedJson}\nactual:   ${actualJson}`);
	}
}

function serializeDefinition(definition: ToolDef): JsonObject {
	const parametersJson = JSON.stringify(definition.parameters);
	return {
		name: definition.name,
		label: definition.label,
		description: definition.description,
		prompt_snippet: definition.promptSnippet,
		prompt_guidelines: definition.promptGuidelines ?? [],
		parameters: definition.parameters,
		parameters_json: parametersJson,
		parameters_sha256: sha256(parametersJson),
	};
}

function normalizePromptPaths(prompt: string): string {
	return prompt
		.replaceAll(getReadmePath(), documentationPaths.readme)
		.replaceAll(getDocsPath(), documentationPaths.docs)
		.replaceAll(getExamplesPath(), documentationPaths.examples);
}

function verifySourceManifest(manifest: JsonObject): void {
	const files = manifest.files;
	if (!Array.isArray(files)) throw new Error("source manifest files must be an array");
	for (const entry of files) {
		if (!entry || typeof entry !== "object") throw new Error("source manifest entry must be an object");
		const file = entry as JsonObject;
		if (typeof file.path !== "string" || typeof file.sha256 !== "string") {
			throw new Error("source manifest entries require path and sha256");
		}
		const sourcePath = resolve(repositoryRoot, "parity/upstream/source", file.path);
		const actualHash = sha256(readFileSync(sourcePath));
		assertEqual(actualHash, file.sha256, `source ${file.path}`);
	}
}

function verifyBehaviorManifest(manifest: JsonObject, standardNames: string[]): void {
	const tools = manifest.tools;
	if (!Array.isArray(tools)) throw new Error("behavior manifest tools must be an array");
	const names = tools.map((tool) => (tool as JsonObject).name);
	assertEqual(names, standardNames, "behavior manifest tool order");
	for (const tool of tools) {
		if (!tool || typeof tool !== "object") throw new Error("behavior manifest tool must be an object");
		const entry = tool as JsonObject;
		if (typeof entry.source !== "string") throw new Error(`missing source for ${String(entry.name)}`);
		if (!Array.isArray(entry.adapter_methods) || entry.adapter_methods.length === 0) {
			throw new Error(`missing adapter methods for ${String(entry.name)}`);
		}
		if (!Array.isArray(entry.cases)) throw new Error(`missing cases for ${String(entry.name)}`);
		const kinds = new Set((entry.cases as JsonObject[]).map((test) => test.kind));
		for (const requiredKind of ["success", "invalid_input", "host_error"]) {
			if (!kinds.has(requiredKind)) throw new Error(`${String(entry.name)} lacks ${requiredKind} case`);
		}
	}
}

async function verifyOperationBoundary(standardNames: string[]): Promise<void> {
	const capture = await captureProfileBehavior();
	const captureTools = capture.tools;
	if (!Array.isArray(captureTools)) throw new Error("profile behavior capture tools must be an array");
	assertEqual(
		captureTools.map((tool) => String((tool as JsonObject).name)),
		standardNames,
		"profile behavior capture tool order",
	);
	for (const tool of captureTools) {
		const entry = tool as JsonObject;
		const name = String(entry.name);
		const cases = entry.cases;
		if (!Array.isArray(cases)) throw new Error(`profile behavior capture has no cases for ${name}`);
		if (cases.length !== 3) throw new Error(`profile behavior capture requires three cases for ${name}`);
		const statuses = cases.map((test) => String((test as JsonObject).status));
		if (name === "grep") {
			assertEqual(statuses, ["blocked", "invalid_input", "blocked"], "profile grep isolation status");
			const blockers = capture.blockers;
			if (!Array.isArray(blockers) || !blockers.includes(grepBlocker)) {
				throw new Error("profile grep blocker changed without an explicit review");
			}
		} else {
			assertEqual(statuses, ["success", "invalid_input", "host_error"], `profile ${name} operation statuses`);
		}
	}
}

const profile = readJson(profilePath);
const sourceManifest = readJson(sourceManifestPath);
const behaviorManifest = readJson(behaviorManifestPath);
const activeDefinitions = createCodingToolDefinitions(workspaceRoot);
const allDefinitions = createAllToolDefinitions(workspaceRoot);
const expectedActive = activeDefinitions.map(serializeDefinition);
const expectedStandard = Object.values(allDefinitions).map(serializeDefinition);

assertEqual(profile.upstream, sourceManifest.upstream, "upstream pin");
verifySourceManifest(sourceManifest);
assertEqual(profile.active_tools, expectedActive, "active tool definitions");
assertEqual(profile.standard_tools, expectedStandard, "standard tool definitions");

const expectedPrompt = normalizePromptPaths(
	buildSystemPrompt({
		cwd: workspaceRoot,
		selectedTools: activeDefinitions.map((definition) => definition.name),
		toolSnippets: Object.fromEntries(
			activeDefinitions.map((definition) => [definition.name, definition.promptSnippet]),
		),
		promptGuidelines: activeDefinitions.flatMap((definition) => definition.promptGuidelines ?? []),
	}),
);
const systemPrompt = profile.system_prompt as JsonObject;
assertEqual(systemPrompt.text, expectedPrompt, "default system prompt text");
assertEqual(systemPrompt.sha256, sha256(expectedPrompt), "default system prompt hash");
verifyBehaviorManifest(behaviorManifest, expectedStandard.map((definition) => String(definition.name)));
await verifyOperationBoundary(expectedStandard.map((definition) => String(definition.name)));

console.log(
	`profile verification passed: ${expectedActive.length} active tools, ${expectedStandard.length} standard tools, ${expectedPrompt.length} prompt bytes; six virtual operation adapters executable, grep explicitly isolated`,
);
