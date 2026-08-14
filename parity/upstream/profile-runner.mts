/**
 * Emit the version-pinned Pi default coding profile without starting the Pi CLI.
 *
 * Run from `parity/upstream/source` after `npm ci --ignore-scripts`:
 *
 *   ./node_modules/.bin/tsx ../profile-runner.mts > ../../profile/default-profile.json
 *
 * The captured source calls Pi's SDK factories directly.  Absolute documentation locations are
 * the only installation-specific values in `buildSystemPrompt`; they are replaced with fixed
 * virtual paths so the resulting fixture is reproducible and the Rust profile need not discover
 * an installation directory.
 */

import { createHash } from "node:crypto";
import { getDocsPath, getExamplesPath, getReadmePath } from "./source/packages/coding-agent/src/config.ts";
import { buildSystemPrompt } from "./source/packages/coding-agent/src/core/system-prompt.ts";
import {
	createAllToolDefinitions,
	createCodingToolDefinitions,
	type ToolDef,
} from "./source/packages/coding-agent/src/core/tools/index.ts";

const workspaceRoot = "/fixture/workspace";
const documentationPaths = {
	readme: "/fixture/pi/README.md",
	docs: "/fixture/pi/docs",
	examples: "/fixture/pi/examples",
} as const;

function sha256(value: string): string {
	return createHash("sha256").update(value, "utf8").digest("hex");
}

function serializeDefinition(definition: ToolDef) {
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

const activeDefinitions = createCodingToolDefinitions(workspaceRoot);
const allDefinitions = createAllToolDefinitions(workspaceRoot);
const prompt = normalizePromptPaths(
	buildSystemPrompt({
		cwd: workspaceRoot,
		selectedTools: activeDefinitions.map((definition) => definition.name),
		toolSnippets: Object.fromEntries(
			activeDefinitions.map((definition) => [definition.name, definition.promptSnippet]),
		),
		promptGuidelines: activeDefinitions.flatMap((definition) => definition.promptGuidelines ?? []),
	}),
);

const fixture = {
	format_version: 1,
	kind: "pinned_default_coding_profile",
	upstream: {
		repository: "https://github.com/earendil-works/pi.git",
		commit: "9d2ec7ffabe927bfad2214c1cee25b6632a78dcf",
		package: "@earendil-works/pi-coding-agent",
		package_version: "0.84.1",
	},
	inputs: {
		workspace_root: workspaceRoot,
		documentation_paths: documentationPaths,
	},
	active_tools: activeDefinitions.map(serializeDefinition),
	standard_tools: Object.values(allDefinitions).map(serializeDefinition),
	system_prompt: {
		text: prompt,
		sha256: sha256(prompt),
	},
};

process.stdout.write(`${JSON.stringify(fixture, null, 2)}\n`);
