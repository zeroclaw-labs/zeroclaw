import "./globals.css";
import { ConvexClientProvider } from "@/components/convex-client";

export const metadata = {
  title: "ClawPilot Workbench",
  description: "Workspace-first coworker flow for runtime-backed knowledge work"
};

export default function RootLayout({ children }) {
  return (
    <html lang="en">
      <body>
        <ConvexClientProvider>{children}</ConvexClientProvider>
      </body>
    </html>
  );
}
