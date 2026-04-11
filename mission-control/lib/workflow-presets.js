export const WORKFLOW_PRESETS = [
  {
    id: "folder-summarization",
    label: "Summarize folder",
    value: "Folder summarization",
    description: "Generate a concise map of purpose, key files, and risks for a folder.",
    goalTemplate:
      "Summarize the folder {{targetPath}}. Include: purpose, key files, architecture notes, and top follow-up actions.",
    instructionTemplate:
      "Prioritize clarity over completeness. Cite exact file paths in the deliverable.",
    requiresTargetPath: true,
    supportsPriorDeliverable: false
  },
  {
    id: "file-organization",
    label: "Organize files",
    value: "File organization / cleanup",
    description: "Propose and apply safe file cleanup and naming improvements.",
    goalTemplate:
      "Organize files under {{targetPath}}. Remove obvious clutter, improve naming consistency, and produce a cleanup summary with reversible steps.",
    instructionTemplate:
      "Do not delete critical source files. Prefer staged, reviewable edits and explain every destructive action.",
    requiresTargetPath: true,
    supportsPriorDeliverable: false
  },
  {
    id: "document-synthesis",
    label: "Synthesize documents",
    value: "Document synthesis",
    description: "Create one coherent document from multiple local source files.",
    goalTemplate:
      "Synthesize a single document from local files in {{targetPath}}. Capture common themes, conflicts, and a unified recommendation.",
    instructionTemplate:
      "Only use local workspace files as sources. Call out uncertainty instead of inventing missing context.",
    requiresTargetPath: true,
    supportsPriorDeliverable: false
  },
  {
    id: "data-extraction",
    label: "Extract messy data",
    value: "Data extraction",
    description: "Extract structured facts from inconsistent local notes, logs, or text files.",
    goalTemplate:
      "Extract structured data from files in {{targetPath}}. Return normalized fields, confidence notes, and unresolved records.",
    instructionTemplate:
      "Prefer deterministic parsing and explicit nulls for missing values. Keep the output easy to validate.",
    requiresTargetPath: true,
    supportsPriorDeliverable: false
  },
  {
    id: "rerun-refine",
    label: "Rerun or refine",
    value: "Rerun / refine prior task",
    description: "Continue from a previous run result and improve the deliverable.",
    goalTemplate:
      "Refine the prior deliverable from run {{runId}}. Address gaps, improve quality, and produce an updated deliverable with a change summary.",
    instructionTemplate:
      "Preserve successful parts of the previous output and focus changes on explicit reviewer concerns.",
    requiresTargetPath: false,
    supportsPriorDeliverable: true
  }
];

export function getWorkflowPreset(presetId) {
  return WORKFLOW_PRESETS.find((preset) => preset.id === presetId) || null;
}

export function buildGoalFromPreset({ presetId, targetPath, runId }) {
  const preset = getWorkflowPreset(presetId);
  if (!preset) {
    return null;
  }

  return preset.goalTemplate
    .replaceAll("{{targetPath}}", targetPath || "the workspace root")
    .replaceAll("{{runId}}", runId || "the latest related run");
}
