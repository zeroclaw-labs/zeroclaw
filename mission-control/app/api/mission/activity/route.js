import { recentActivity } from "@/lib/server/mission-store";

export async function GET(request) {
  const { searchParams } = new URL(request.url);
  const limit = Number.parseInt(searchParams.get("limit") || "20", 10);
  const entries = await recentActivity(limit);
  return Response.json({ entries });
}
