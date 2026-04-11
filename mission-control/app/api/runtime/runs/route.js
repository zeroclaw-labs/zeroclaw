import { randomUUID } from "node:crypto";
import { mkdir, readdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";

function queueRoot() {
  return process.env.RUNTIME_QUEUE_ROOT || "/var/lib/clawpilot/queue";
}

function resultsRoot() {
  return process.env.RUNTIME_RESULTS_ROOT || "/var/lib/clawpilot/results";
}

export async function GET() {
  const root = resultsRoot();
  await mkdir(root, { recursive: true });
  const entries = await readdir(root);
  const statusFiles = entries.filter((name) => name.endsWith(".status.json"));
  const runs = await Promise.all(
    statusFiles.map(async (file) => {
      const fullPath = path.join(root, file);
      const body = await readFile(fullPath, "utf8");
      return JSON.parse(body);
    })
  );
  runs.sort((a, b) => (b.created_at || "").localeCompare(a.created_at || ""));
  return Response.json({ runs });
}

export async function POST(request) {
  const body = await request.json();
  const goal = String(body.goal || "").trim();
  const workspacePath = String(body.workspacePath || "").trim();
  const agent = String(body.agent || "default").trim();
  if (!goal || !workspacePath) {
    return Response.json(
      { error: "goal and workspacePath are required" },
      { status: 400 }
    );
  }

  const runId = randomUUID();
  const createdAt = new Date().toISOString();
  const queueDir = path.join(queueRoot(), agent);
  await mkdir(queueDir, { recursive: true });
  const queueFile = path.join(queueDir, `${runId}.json`);

  const payload = {
    id: runId,
    agent,
    text: goal,
    created_at: createdAt,
    workspace_path: workspacePath,
    global_instructions: body.globalInstructions || "",
    folder_instructions: Array.isArray(body.folderInstructions)
      ? body.folderInstructions.map((item) => ({
          folder_path: String(item.folderPath || ""),
          instructions: String(item.instructions || "")
        }))
      : []
  };

  await writeFile(queueFile, JSON.stringify(payload, null, 2));

  return Response.json({ runId, queueFile, createdAt });
}
