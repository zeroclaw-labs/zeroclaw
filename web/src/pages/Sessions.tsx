import { SessionsTab } from "./Dashboard";
import { t } from "@/lib/i18n";

export default function Sessions() {
  return (
    <div className="mx-auto max-w-6xl p-4 sm:p-8 space-y-5">
      <h2 className="text-2xl font-semibold">{t("home.sessions")}</h2>
      <SessionsTab />
    </div>
  );
}
