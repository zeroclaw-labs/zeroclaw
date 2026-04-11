import { dashboardData } from "@/lib/server/mission-store";

export async function GET(request) {
  const { searchParams } = new URL(request.url);
  const workspaceId = searchParams.get("workspaceId") || undefined;
  const dashboard = await dashboardData(workspaceId);
  return Response.json(dashboard);
}
