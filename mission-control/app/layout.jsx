import "./globals.css";
import { ConvexClientProvider } from "@/components/convex-client";

export const metadata = {
  title: "Mission Control",
  description: "Project Workspace-centered planning and execution"
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
