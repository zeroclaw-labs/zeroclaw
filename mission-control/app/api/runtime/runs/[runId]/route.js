import { readFile } from "node:fs/promises";
import path from "node:path";

function resultsRoot() {
  return process.env.RUNTIME_RESULTS_ROOT || "/var/lib/clawpilot/results";
}

async function maybeReadJson(filePath) {
  try {
    const body = await readFile(filePath, "utf8");
    return JSON.parse(body);
  } catch (_error) {
    return null;
  }
}

async function readEvents(filePath) {
  try {
    const body = await readFile(filePath, "utf8");
    return body
      .split("\n")
      .map((line) => line.trim())
      .filter(Boolean)
      .map((line) => JSON.parse(line));
  } catch (_error) {
    return [];
  }
}

export async function GET(_request, { params }) {
  const { runId } = params;
  const root = resultsRoot();

  const [state, result, events] = await Promise.all([
    maybeReadJson(path.join(root, `${runId}.status.json`)),
    maybeReadJson(path.join(root, `${runId}.json`)),
    readEvents(path.join(root, `${runId}.events.jsonl`))
  ]);

  if (!state && !result) {
    return Response.json({ error: "run not found" }, { status: 404 });
  }

  return Response.json({ state, result, events });
}
