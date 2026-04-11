import "./globals.css";
export const metadata = {
  title: "ClawPilot Workbench",
  description: "Workspace-first coworker flow for runtime-backed knowledge work"
};

export default function RootLayout({ children }) {
  return (
    <html lang="en">
      <body>
        {children}
      </body>
    </html>
  );
}
