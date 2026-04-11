import { seedMissionData } from "@/lib/server/mission-store";

export async function POST() {
  const result = await seedMissionData();
  return Response.json(result);
}
