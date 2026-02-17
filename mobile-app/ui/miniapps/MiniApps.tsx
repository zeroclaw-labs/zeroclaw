import React, { useMemo, useState } from "react";
import { View, Pressable, Text as RNText, StyleSheet, Platform, type ViewStyle } from "react-native";
import { Ionicons } from "@expo/vector-icons";

export type MiniAppKind = "checklist" | "booking" | "counter" | "notes" | "expenses";

type MiniAppVariant = "card" | "fill";

const palette = {
  bg: "#0B1220",
  grouped: "#0F172A",
  separator: "#1F2937",
  text: "#E5E7EB",
  muted: "#94A3B8",
  tint: Platform.OS === "ios" ? "#3B82F6" : "#2563EB",
  success: "#22C55E",
  danger: "#EF4444",
};

export function MiniApp({
  kind,
  variant = "card",
  style,
  showTabs,
}: {
  kind: MiniAppKind;
  variant?: MiniAppVariant;
  style?: ViewStyle;
  showTabs?: boolean;
}) {
  const tabs = showTabs ?? variant === "fill";
  if (kind === "checklist") return <MiniChecklist variant={variant} style={style} showTabs={tabs} />;
  if (kind === "booking") return <MiniBooking variant={variant} style={style} showTabs={tabs} />;
  if (kind === "notes") return <MiniNotes variant={variant} style={style} showTabs={tabs} />;
  if (kind === "expenses") return <MiniExpenses variant={variant} style={style} showTabs={tabs} />;
  return <MiniCounter variant={variant} style={style} showTabs={tabs} />;
}

function MiniChecklist({ variant, style, showTabs }: { variant: MiniAppVariant; style?: ViewStyle; showTabs: boolean }) {
  const [tab, setTab] = useState<"list" | "settings">("list");
  const [items, setItems] = useState([
    { id: "1", text: "Milk", done: false },
    { id: "2", text: "Eggs", done: true },
    { id: "3", text: "Bread", done: false }
  ]);

  return (
    <Shell
      title="Shopping"
      subtitle="Tap to toggle"
      icon="checkmark-circle"
      variant={variant}
      style={style}
      showTabs={showTabs}
      tabs={[
        { key: "list", label: "List", icon: "list" },
        { key: "settings", label: "Settings", icon: "options" },
      ]}
      activeTab={tab}
      onTabChange={(k) => setTab(k as any)}
    >
      {tab === "list" ? (
        <View style={{ gap: 10 }}>
          {items.map((it) => (
            <Pressable
              key={it.id}
              onPress={() => setItems((prev) => prev.map((p) => (p.id === it.id ? { ...p, done: !p.done } : p)))}
              style={({ pressed }) => [styles.row, { opacity: pressed ? 0.88 : 1 }]}
            >
              <RNText style={styles.rowText}>{it.text}</RNText>
              <Ionicons name={it.done ? "checkmark" : "ellipse-outline"} size={18} color={it.done ? palette.success : palette.muted} />
            </Pressable>
          ))}
        </View>
      ) : (
        <View style={{ gap: 10 }}>
          <View style={styles.summaryCard}>
            <RNText style={styles.cardTitle}>Share</RNText>
            <RNText style={styles.meta}>Invite a friend to add items.</RNText>
          </View>
          <View style={styles.summaryCard}>
            <RNText style={styles.cardTitle}>Notifications</RNText>
            <RNText style={styles.meta}>Get reminded when you're near a store.</RNText>
          </View>
        </View>
      )}
    </Shell>
  );
}

function MiniBooking({ variant, style, showTabs }: { variant: MiniAppVariant; style?: ViewStyle; showTabs: boolean }) {
  const [tab, setTab] = useState<"slots" | "details">("slots");
  const slots = useMemo(() => ["10:00", "11:30", "13:00", "15:00"], []);
  const [selected, setSelected] = useState<string | null>("11:30");

  return (
    <Shell
      title="Booking"
      subtitle="Pick a slot"
      icon="calendar"
      variant={variant}
      style={style}
      showTabs={showTabs}
      tabs={[
        { key: "slots", label: "Slots", icon: "time" },
        { key: "details", label: "Details", icon: "information-circle" },
      ]}
      activeTab={tab}
      onTabChange={(k) => setTab(k as any)}
    >
      {tab === "slots" ? (
        <View>
          <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 10 }}>
            {slots.map((s) => {
              const active = selected === s;
              return (
                <Pressable
                  key={s}
                  onPress={() => setSelected(s)}
                  style={({ pressed }) => [
                    styles.chip,
                    {
                      backgroundColor: active ? palette.tint : palette.bg,
                      borderColor: active ? palette.tint : palette.separator,
                      opacity: pressed ? 0.88 : 1
                    }
                  ]}
                >
                  <RNText style={[styles.chipText, { color: active ? "#FFFFFF" : palette.text }]}>{s}</RNText>
                </Pressable>
              );
            })}
          </View>
          <RNText style={[styles.meta, { marginTop: 12 }]}>Selected: {selected ?? "-"}</RNText>
        </View>
      ) : (
        <View style={{ gap: 10 }}>
          <View style={styles.summaryCard}>
            <RNText style={styles.cardTitle}>Studio</RNText>
            <RNText style={styles.meta}>Afterhours — downtown</RNText>
          </View>
          <View style={styles.summaryCard}>
            <RNText style={styles.cardTitle}>Duration</RNText>
            <RNText style={styles.meta}>45 min · instant confirmation</RNText>
          </View>
        </View>
      )}
    </Shell>
  );
}

function MiniCounter({ variant, style, showTabs }: { variant: MiniAppVariant; style?: ViewStyle; showTabs: boolean }) {
  const [tab, setTab] = useState<"today" | "stats">("today");
  const [count, setCount] = useState(7);
  return (
    <Shell
      title="Streak"
      subtitle="Tiny habit counter"
      icon="flame"
      variant={variant}
      style={style}
      showTabs={showTabs}
      tabs={[
        { key: "today", label: "Today", icon: "flame" },
        { key: "stats", label: "Stats", icon: "stats-chart" },
      ]}
      activeTab={tab}
      onTabChange={(k) => setTab(k as any)}
    >
      {tab === "today" ? (
        <View style={{ flexDirection: "row", alignItems: "center", justifyContent: "space-between" }}>
          <RNText style={styles.bigNumber}>{count}</RNText>
          <View style={{ flexDirection: "row", gap: 10 }}>
            <Pressable onPress={() => setCount((c) => Math.max(0, c - 1))} style={({ pressed }) => [styles.iconButton, { opacity: pressed ? 0.85 : 1 }]}>
              <Ionicons name="remove" size={18} color={palette.text} />
            </Pressable>
            <Pressable onPress={() => setCount((c) => c + 1)} style={({ pressed }) => [styles.iconButton, styles.iconButtonPrimary, { opacity: pressed ? 0.85 : 1 }]}>
              <Ionicons name="add" size={18} color="#FFFFFF" />
            </Pressable>
          </View>
        </View>
      ) : (
        <View style={{ gap: 10 }}>
          <View style={styles.summaryCard}>
            <RNText style={styles.cardTitle}>Longest streak</RNText>
            <RNText style={styles.meta}>14 days</RNText>
          </View>
          <View style={styles.summaryCard}>
            <RNText style={styles.cardTitle}>This week</RNText>
            <RNText style={styles.meta}>5 / 7 days</RNText>
          </View>
        </View>
      )}
    </Shell>
  );
}

function MiniNotes({ variant, style, showTabs }: { variant: MiniAppVariant; style?: ViewStyle; showTabs: boolean }) {
  const [tab, setTab] = useState<"inbox" | "pinned">("inbox");
  const [notes, setNotes] = useState([
    { id: "n1", title: "Launch checklist", body: "Landing page, screenshots, pricing", pinned: true },
    { id: "n2", title: "UX polish", body: "Tighten spacing, fix safe areas", pinned: false },
    { id: "n3", title: "Ideas", body: "Mini calendar + reminders", pinned: false },
  ]);

  return (
    <Shell
      title="Notes"
      subtitle="Quick capture"
      icon="document-text"
      variant={variant}
      style={style}
      showTabs={showTabs}
      tabs={[
        { key: "inbox", label: "Inbox", icon: "mail" },
        { key: "pinned", label: "Pinned", icon: "pin" },
      ]}
      activeTab={tab}
      onTabChange={(k) => setTab(k as any)}
    >
      <View style={{ gap: 10 }}>
        {notes
          .filter((n) => (tab === "pinned" ? n.pinned : true))
          .map((n) => (
            <Pressable
              key={n.id}
              onPress={() => setNotes((prev) => prev.map((p) => (p.id === n.id ? { ...p, pinned: !p.pinned } : p)))}
              style={({ pressed }) => [styles.cardRow, { opacity: pressed ? 0.88 : 1 }]}
            >
              <View style={{ flex: 1 }}>
                <RNText style={styles.cardTitle} numberOfLines={1}>
                  {n.title}
                </RNText>
                <RNText style={styles.meta} numberOfLines={1}>
                  {n.body}
                </RNText>
              </View>
              <Ionicons name={n.pinned ? "pin" : "pin-outline"} size={18} color={n.pinned ? palette.tint : palette.muted} />
            </Pressable>
          ))}
        <View style={styles.compose}>
          <Ionicons name="add" size={18} color={palette.text} />
          <RNText style={styles.composeText}>New note</RNText>
        </View>
      </View>
    </Shell>
  );
}

function MiniExpenses({ variant, style, showTabs }: { variant: MiniAppVariant; style?: ViewStyle; showTabs: boolean }) {
  const [tab, setTab] = useState<"overview" | "recent">("overview");
  const [month] = useState("Feb");
  const [spent] = useState(482);
  const [budget] = useState(650);
  const pct = Math.min(1, spent / budget);

  return (
    <Shell
      title="Budget"
      subtitle="This month"
      icon="wallet"
      variant={variant}
      style={style}
      showTabs={showTabs}
      tabs={[
        { key: "overview", label: "Overview", icon: "pie-chart" },
        { key: "recent", label: "Recent", icon: "list" },
      ]}
      activeTab={tab}
      onTabChange={(k) => setTab(k as any)}
    >
      {tab === "overview" ? (
        <View style={{ gap: 10 }}>
          <View style={styles.summaryCard}>
            <View style={{ flexDirection: "row", justifyContent: "space-between", alignItems: "baseline" }}>
              <RNText style={styles.cardTitle}>{month} Spend</RNText>
              <RNText style={styles.summaryValue}>${spent}</RNText>
            </View>
            <RNText style={styles.meta}>Budget: ${budget}</RNText>
            <View style={styles.progressTrack}>
              <View style={[styles.progressFill, { width: `${Math.round(pct * 100)}%`, backgroundColor: pct > 0.85 ? palette.danger : palette.tint }]} />
            </View>
          </View>

          <View style={styles.summaryCard}>
            <RNText style={styles.cardTitle}>Left</RNText>
            <RNText style={styles.meta}>${Math.max(0, budget - spent).toFixed(0)} remaining</RNText>
          </View>
        </View>
      ) : (
        <View style={{ gap: 8 }}>
          <Txn icon="cart" label="Groceries" amount={-64.2} />
          <Txn icon="cafe" label="Coffee" amount={-8.5} />
          <Txn icon="cash" label="Refund" amount={+19.0} positive />
        </View>
      )}
    </Shell>
  );
}

function Txn({
  icon,
  label,
  amount,
  positive,
}: {
  icon: keyof typeof Ionicons.glyphMap;
  label: string;
  amount: number;
  positive?: boolean;
}) {
  return (
    <View style={styles.txnRow}>
      <View style={styles.txnIcon}>
        <Ionicons name={icon} size={16} color={palette.muted} />
      </View>
      <RNText style={styles.rowText}>{label}</RNText>
      <RNText style={[styles.txnAmount, { color: positive ? palette.success : palette.text }]}>${amount.toFixed(2)}</RNText>
    </View>
  );
}

function Shell({
  title,
  subtitle,
  icon,
  children,
  variant,
  style,
  tabs,
  activeTab,
  onTabChange,
  showTabs = true
}: {
  title: string;
  subtitle: string;
  icon: keyof typeof Ionicons.glyphMap;
  children: React.ReactNode;
  variant: MiniAppVariant;
  style?: ViewStyle;
  tabs?: Array<{ key: string; label: string; icon: keyof typeof Ionicons.glyphMap }>;
  activeTab?: string;
  onTabChange?: (key: string) => void;
  showTabs?: boolean;
}) {
  return (
    <View style={[styles.shell, variant === "fill" ? styles.shellFill : styles.shellCard, style]}>
      <View style={styles.nav}>
        <View style={styles.appIcon}>
          <Ionicons name={icon} size={18} color={palette.tint} />
        </View>
        <View style={{ flex: 1 }}>
          <RNText style={styles.navTitle}>{title}</RNText>
          <RNText style={styles.navSubtitle}>{subtitle}</RNText>
        </View>
        <Ionicons name="ellipsis-horizontal" size={18} color={palette.muted} />
      </View>
      <View style={styles.content}>{children}</View>

      {showTabs && tabs && tabs.length > 0 ? (
        <View style={styles.tabBar}>
          {tabs.map((t) => {
            const active = t.key === activeTab;
            return (
              <Pressable
                key={t.key}
                onPress={() => onTabChange?.(t.key)}
                style={({ pressed }) => [styles.tabItem, { opacity: pressed ? 0.8 : 1 }]}
              >
                <Ionicons name={t.icon} size={18} color={active ? palette.tint : palette.muted} />
                <RNText style={[styles.tabLabel, { color: active ? palette.tint : palette.muted }]}>{t.label}</RNText>
              </Pressable>
            );
          })}
        </View>
      ) : null}
    </View>
  );
}

const styles = StyleSheet.create({
  shell: {
    backgroundColor: palette.bg,
    overflow: "hidden",
  },
  shellCard: {
    borderRadius: 24,
    borderWidth: 1,
    borderColor: palette.separator,
  },
  shellFill: {
    flex: 1,
    borderRadius: 0,
  },
  nav: {
    height: 56,
    paddingHorizontal: 16,
    flexDirection: "row",
    alignItems: "center",
    gap: 12,
    borderBottomWidth: 1,
    borderBottomColor: palette.separator,
    backgroundColor: palette.bg,
  },
  appIcon: {
    width: 34,
    height: 34,
    borderRadius: 10,
    backgroundColor: palette.grouped,
    borderWidth: 1,
    borderColor: palette.separator,
    alignItems: "center",
    justifyContent: "center",
  },
  navTitle: {
    fontSize: 16,
    fontWeight: "600",
    color: palette.text,
  },
  navSubtitle: {
    marginTop: 2,
    fontSize: 12,
    color: palette.muted,
  },
  content: {
    padding: 16,
    backgroundColor: palette.grouped,
    flex: 1,
    gap: 10,
  },

  tabBar: {
    height: 60,
    paddingHorizontal: 16,
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-around",
    borderTopWidth: 1,
    borderTopColor: palette.separator,
    backgroundColor: palette.bg,
  },
  tabItem: {
    height: 60,
    alignItems: "center",
    justifyContent: "center",
    gap: 4,
  },
  tabLabel: {
    fontSize: 11,
    fontWeight: "600",
  },
  row: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    paddingVertical: 12,
    paddingHorizontal: 14,
    borderRadius: 16,
    backgroundColor: palette.bg,
    borderWidth: 1,
    borderColor: palette.separator,
  },
  rowText: {
    fontSize: 15,
    color: palette.text,
  },
  chip: {
    paddingVertical: 10,
    paddingHorizontal: 12,
    borderRadius: 14,
    borderWidth: 1,
  },
  chipText: {
    fontSize: 13,
    fontWeight: "600",
  },
  meta: {
    fontSize: 13,
    color: palette.muted,
  },
  bigNumber: {
    fontSize: 44,
    fontWeight: "700",
    color: palette.text,
  },
  iconButton: {
    width: 44,
    height: 44,
    borderRadius: 14,
    backgroundColor: palette.bg,
    borderWidth: 1,
    borderColor: palette.separator,
    alignItems: "center",
    justifyContent: "center",
  },
  iconButtonPrimary: {
    backgroundColor: palette.tint,
    borderColor: palette.tint,
  },

  sectionTitle: {
    marginTop: 2,
    fontSize: 12,
    letterSpacing: 0.6,
    textTransform: "uppercase",
    color: palette.muted,
  },
  cardRow: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    paddingVertical: 12,
    paddingHorizontal: 14,
    borderRadius: 16,
    backgroundColor: palette.bg,
    borderWidth: 1,
    borderColor: palette.separator,
    gap: 12,
  },
  cardTitle: {
    fontSize: 15,
    fontWeight: "600",
    color: palette.text,
  },
  compose: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "center",
    gap: 8,
    paddingVertical: 12,
    borderRadius: 16,
    borderWidth: 1,
    borderStyle: "dashed",
    borderColor: palette.separator,
    backgroundColor: "rgba(255,255,255,0.02)",
  },
  composeText: {
    fontSize: 14,
    fontWeight: "600",
    color: palette.text,
  },
  summaryCard: {
    padding: 14,
    borderRadius: 18,
    backgroundColor: palette.bg,
    borderWidth: 1,
    borderColor: palette.separator,
    gap: 8,
  },
  summaryValue: {
    fontSize: 22,
    fontWeight: "700",
    color: palette.text,
  },
  progressTrack: {
    height: 10,
    borderRadius: 10,
    backgroundColor: "rgba(255,255,255,0.06)",
    overflow: "hidden",
  },
  progressFill: {
    height: 10,
    borderRadius: 10,
  },
  txnRow: {
    flexDirection: "row",
    alignItems: "center",
    gap: 10,
    paddingVertical: 12,
    paddingHorizontal: 14,
    borderRadius: 16,
    backgroundColor: palette.bg,
    borderWidth: 1,
    borderColor: palette.separator,
  },
  txnIcon: {
    width: 28,
    height: 28,
    borderRadius: 10,
    backgroundColor: "rgba(255,255,255,0.04)",
    alignItems: "center",
    justifyContent: "center",
    borderWidth: 1,
    borderColor: palette.separator,
  },
  txnAmount: {
    marginLeft: "auto",
    fontSize: 14,
    fontWeight: "700",
  },
});
