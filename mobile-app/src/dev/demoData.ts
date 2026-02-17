import type { GalleryItem, Project } from "../api/platform";
import type { ChatMessage } from "../state/chat";

let demoProjectsState: Project[] = [
  {
    id: "demo_proj_tasks",
    name: "Shared Shopping List",
    visibility: "private",
    template: "List & Tasks",
    theme: "Tech",
    latest_snapshot_id: "snap_demo_001"
  },
  {
    id: "demo_proj_booking",
    name: "Studio Booking",
    visibility: "private",
    template: "Booking",
    theme: "Luxury",
    latest_snapshot_id: "snap_demo_014"
  },
  {
    id: "demo_proj_media",
    name: "Mini Podcast",
    visibility: "private",
    template: "Content/Media",
    theme: "Pastel",
    latest_snapshot_id: "snap_demo_021"
  }
];

export const getDemoProjects = () => demoProjectsState;

export const addDemoProject = (p: Project) => {
  demoProjectsState = [p, ...demoProjectsState];
};

export const demoGallery: GalleryItem[] = [
  {
    project_id: "gal_demo_notes_01",
    name: "Night Notes",
    template: "Notes",
    theme: "Dark"
  },
  {
    project_id: "gal_demo_budget_02",
    name: "Pocket Budget",
    template: "Finance",
    theme: "Dark"
  },
  {
    project_id: "gal_demo_booking_03",
    name: "Afterhours Booking",
    template: "Booking",
    theme: "Dark"
  },
  {
    project_id: "gal_demo_tasks_04",
    name: "Grocery Sprint",
    template: "List & Tasks",
    theme: "Dark"
  },
  {
    project_id: "gal_demo_counter_05",
    name: "Streak Counter",
    template: "Habit",
    theme: "Dark"
  }
];

export const demoChats: Record<string, ChatMessage[]> = {
  demo_proj_tasks: [
    { id: "m1", role: "user", text: "I want a shopping list with sharing.", ts: 1 },
    { id: "a1", role: "assistant", text: "Got it. I'll make it easy to add items and share.", ts: 2, meta: { snapshot_id: "snap_demo_001" } },
    { id: "m2", role: "user", text: "Add categories and a quick add button.", ts: 3 },
    { id: "a2", role: "assistant", text: "Done. Categories are now one tap away.", ts: 4, meta: { snapshot_id: "snap_demo_002" } }
  ],
  demo_proj_booking: [
    { id: "m1", role: "user", text: "A booking app for a tattoo studio.", ts: 1 },
    { id: "a1", role: "assistant", text: "Nice. I'll add services, time slots, and confirmations.", ts: 2, meta: { snapshot_id: "snap_demo_014" } },
    { id: "m2", role: "user", text: "Make it feel premium.", ts: 3 },
    { id: "a2", role: "assistant", text: "Updated the look to feel more premium.", ts: 4, meta: { snapshot_id: "snap_demo_015" } }
  ],
  demo_proj_media: [
    { id: "m1", role: "user", text: "A mini podcast library with playlists.", ts: 1 },
    { id: "a1", role: "assistant", text: "Perfect. I'll add a library, playlists, and a player.", ts: 2, meta: { snapshot_id: "snap_demo_021" } }
  ]
};
