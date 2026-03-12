export default function ChatLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  // Chat uses its own full-screen layout without the global header.
  // The Header component already hides itself on /chat routes.
  return <>{children}</>;
}
