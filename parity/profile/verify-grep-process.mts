/** Verify the one pinned standard-tool factory that necessarily owns a process seam. */

import { capture } from "../upstream/profile-grep-process-runner.mts";

function equal(actual: unknown, expected: unknown, label: string): void {
	const actualJson = JSON.stringify(actual);
	const expectedJson = JSON.stringify(expected);
	if (actualJson !== expectedJson) {
		throw new Error(`${label} mismatch\nexpected: ${expectedJson}\nactual:   ${actualJson}`);
	}
}

const result = await capture();
equal(result.format_version, 1, "grep process capture format");
equal(result.kind, "pinned_profile_grep_process_capture", "grep process capture kind");
equal(result.rg_path, "rg", "grep process resolver");
equal(result.tools.map((tool) => tool.name), ["grep"], "grep process tool name");

const [grep] = result.tools;
equal(
	grep.cases.map((test) => test.status),
	["success", "invalid_input", "host_error"],
	"grep process case statuses",
);
equal(
	grep.cases[0],
	{
		id: "success-context",
		status: "success",
		input: { pattern: "needle", path: "notes.txt", context: 1 },
		content: [
			{
				type: "text",
				text: "notes.txt-1- before\nnotes.txt:2: needle\nnotes.txt-3- needle suffix\nnotes.txt-2- needle\nnotes.txt:3: needle suffix\nnotes.txt-4- after",
			},
		],
		details: null,
	},
	"grep process success result",
);
equal(
	grep.cases[1]?.error?.includes("pattern"),
	true,
	"grep process invalid-input diagnostic",
);
equal(
	grep.cases[2],
	{
		id: "host-missing-path",
		status: "host_error",
		input: { pattern: "needle", path: "missing" },
		error: "Path not found: /fixture/workspace/missing",
	},
	"grep process host-error result",
);

console.log("profile grep process verification passed: pinned factory resolved PATH rg from an empty agent directory");
