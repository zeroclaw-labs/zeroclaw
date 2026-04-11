import { runMissionCommand } from "@/lib/server/mission-store";

export async function POST(request) {
  const body = await request.json();
  const action = String(body.action || "").trim();
  const args = body.args && typeof body.args === "object" ? body.args : {};

  if (!action) {
    return Response.json({ error: "action is required" }, { status: 400 });
  }

  try {
    const result = await runMissionCommand(action, args);
    return Response.json({ ok: true, result });
  } catch (error) {
    return Response.json({ error: error.message || "command failed" }, { status: 400 });
  }
}
