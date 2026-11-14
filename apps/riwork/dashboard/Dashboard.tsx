import { Scroll, Text, View, cleanup, computed, signal, windowSize, type JSX, type ViewRef } from "nsh";
import { Button, Input } from "nsh/base";
import { Icon } from "../Icon";
import { Segmented, StatusPill, buttonLook, focusRing, searchBox, type Tone } from "../PanelKit";
import { DecisionReviewView, createDecisionReview, type DecisionApi, type DecisionReview } from "./DecisionReview";
import type { DashboardDto } from "../client/types";
import { space, type InterfaceTheme } from "../theme";
import {
  COLUMN_WIDTHS, DASHBOARD_METRICS, FILTERS, LIST_LIMIT, SORTS, age, agentParts, attentionCopy, automationDetail,
  chatCopy, emptyTable, filterCounts, filterLabel, freshness, intelligenceSection, layoutFor, orchestratorWords,
  matches, plural, projectReaderName, projectState, quietAttention, sessionName, sessionsLine, sortLabel, tasksWords,
  visibleProjects, workDetail, type DecisionRow, type Filter, type Plan, type ProjectRow, type Sort, type Target, type Wire,
} from "./model";

type Dto = DashboardDto;

export const SEARCH_PLACEHOLDER = "Search projects, tasks and sessions  ⌘F";
const TOUCH_SEARCH_PLACEHOLDER = "Search projects, tasks and sessions";

/** The global dashboard (PANEL-078..099). It draws what the native read says and
 * asks for nothing while drawing: every click is an explicit route or refresh.
 * A decision unfolds its review in its own row; nothing is laid over the window. */
export function DashboardView(props: {
  dto: () => Dto | null;
  theme: () => InterfaceTheme;
  filter: () => Filter;
  sort: () => Sort;
  query: { (): string; set: (value: string) => void };
  onFilter: (filter: Filter) => void;
  onSort: (sort: Sort) => void;
  onSearch: (node: { focus: () => void }) => void;
  onOpen: (target: Target) => void;
  onPanel: (projectId: string, panel: string) => void;
  onGlobalChat: () => void;
  onRefresh: () => void;
  plan: () => Plan | null;
  starting: () => boolean;
  /** The host's review calls (`Services.dashboard.decisions`; a fixture passes its own). */
  decisions: DecisionApi;
  /** Touch density (iPhone, iPad): 44-point targets, no keyboard hints, and on a
   * phone-wide view each project's figures under its name instead of in columns. */
  touch?: boolean;
}) {
  const palette = () => props.theme().palette;
  const scale = () => props.theme().scale;
  const size = (value: number) => space(value, scale());
  const width = signal(0);
  const font = (size: number, color = palette().text) => ({ fontFamily: props.theme().family, fontSize: (size + 1) * scale(), color });
  const mono = (size: number, color = palette().text) => ({ fontFamily: props.theme().monoFamily, fontSize: size * scale(), color });
  const wire = () => (props.dto()?.wire ?? null) as Wire | null;
  const now = () => props.dto()?.now ?? 0;
  const layout = () => layoutFor(width() || windowSize().width, scale());
  const touch = () => props.touch === true;
  /** Below 600 points (`PhoneShell`) the table's columns do not fit. */
  const stacked = () => (width() || windowSize().width) / Math.max(scale(), 1) < 700;
  const touchTarget = () => touch() ? { minHeight: 44, justifyContent: "center" as const } : {};
  // The body is drawn again only when this changes, not on every read or resize.
  const arrangement = computed(() => layout().arrangement);
  let search: ViewRef | undefined;
  const searchFocused = signal(false);
  props.onSearch({ focus: () => search?.focus() });
  // The unfolded review lives here, not in its row, which an arrangement or a
  // compact region change makes again.
  const review = signal<DecisionReview | null>(null);
  cleanup(() => review()?.dispose());
  function fold() { review()?.dispose(); review.set(null); }
  /** A row's Review unfolds it; again, or Close, folds it; another row switches. */
  function unfold(decision: DecisionRow) {
    const current = review();
    if (current?.busy()) return;
    current?.dispose();
    review.set(current?.decision.id === decision.id ? null : createDecisionReview(decision, { api: props.decisions, onResponded: props.onRefresh }));
  }

  /** A control: `plain` is an icon or words with no edge until hovered, `outlined` a button
   * (the card color with a hairline) that must read as one among status words, `primary` the
   * dashboard's one filled action. Without `onPress` it is drawn disabled. */
  function button(id: string, label: string, onPress: (() => void) | undefined, selected = false, hint?: string, kind: "plain" | "outlined" | "primary" = "plain") {
    const icon = label === "↻" || label === "×";
    const look = buttonLook(props.theme, kind === "primary" ? "primary" : "default", !onPress);
    const framed = kind !== "plain";
    return <View testID={id} role="button" accessibilityLabel={hint ?? label} accessibilityState={{ selected, disabled: !onPress }} disabled={!onPress} onPress={onPress}
      focusable={!!onPress} hoverStyle={onPress ? framed ? look.hoverStyle : { backgroundColor: palette().panelActive } : undefined} focusStyle={focusRing(palette(), framed ? 2 : 1)}
      style={{ ...(framed ? { ...look.style, minHeight: size(touch() ? 44 : 24), paddingHorizontal: size(9) } : { paddingHorizontal: size(icon ? 5 : 8), paddingVertical: size(4), borderRadius: size(8), ...touchTarget() }),
        backgroundColor: framed ? look.style.backgroundColor : selected ? palette().bg : "transparent", opacity: onPress ? 1 : 0.5 }}>
      {icon ? <Icon name={label === "↻" ? "refresh" : "close"} size={(label === "↻" ? 13 : 11) * scale()} color={palette().muted} /> : <Text style={{ ...(framed ? { ...look.label, fontSize: 10.5 * scale() } : font(10, selected ? palette().text : palette().muted)), numberOfLines: 1 }}>{label}</Text>}
    </View>;
  }
  function section(id: string, title: string, children: JSX.Element | JSX.Element[], meta?: string) {
    return <View testID={id} style={{ gap: size(3), paddingVertical: size(6), minWidth: 0 }}>
      <View style={{ flexDirection: "row", justifyContent: "space-between", paddingHorizontal: size(8), paddingVertical: size(4) }}>
        <Text style={{ ...font(12), fontWeight: "700" }}>{title}</Text>
        {meta && <Text style={mono(10, palette().muted)}>{meta}</Text>}
      </View>
      {children}
    </View>;
  }
  /** A rail row: a headline and its detail. `state` leads the headline with a pill (what
   * needs the person), instead of coloring the words. */
  function row(id: string, headline: string, detail: string, target?: Target, tone = palette().text, state?: { tone: Tone; label: string }) {
    return <View testID={id} role={target ? "button" : "text"} accessibilityLabel={`${state ? `${state.label} · ` : ""}${headline}; ${detail}`} onPress={target ? () => props.onOpen(target) : undefined} focusable={!!target}
      hoverStyle={{ backgroundColor: palette().panelActive }} focusStyle={focusRing(palette(), -2)} style={{ gap: size(3), paddingHorizontal: size(8), paddingVertical: size(6), borderRadius: size(8), ...(target ? touchTarget() : {}) }}>
      {state ? <View style={{ flexDirection: "row", alignItems: "flex-start", gap: size(8) }}>
        <View style={{ paddingTop: size(1) }}><StatusPill theme={props.theme} tone={state.tone} label={state.label} /></View>
        <Text style={{ ...font(12, tone), flex: 1, minWidth: 0, numberOfLines: 3 }}>{headline}</Text>
      </View> : <Text style={{ ...font(12, tone), numberOfLines: 3 }}>{headline}</Text>}
      <Text style={{ ...font(11, palette().muted), numberOfLines: 3 }}>{detail}</Text>
    </View>;
  }
  /** A rail's line of words, on the same left edge as its title and rows. */
  const note = (tone = palette().muted) => ({ ...font(10, tone), paddingHorizontal: size(8), paddingVertical: size(4) });
  function more(total: number | undefined, shown: number) {
    return total != null && total > shown ? <Text style={note()}>{`${total - shown} more`}</Text> : null;
  }
  function list<T>(rows: T[] | undefined, total: number | undefined, draw: (item: T, index: number) => JSX.Element, empty: string, fields: (item: T) => string[], limit = LIST_LIMIT) {
    const found = (rows ?? []).map((item, index) => ({ item, index })).filter(({ item }) => matches(props.query(), fields(item)));
    const items = found.slice(0, limit);
    const count = props.query().trim() ? found.length : total ?? rows?.length;
    return items.length ? <>{items.map(({ item, index }) => draw(item, index))}{more(count, items.length)}</> :
      <>{count ? more(count, 0) : <Text style={note()}>{rows?.length && props.query().trim() ? `Nothing here matches “${props.query().trim()}”.` : empty}</Text>}</>;
  }
  const sessionFields = (session: { agent: string; title?: string; project_name?: string; branch?: string }) => [session.agent, session.title ?? "", session.project_name ?? "Global", session.branch ?? ""];

  // ---- Regions ---------------------------------------------------------------------------
  function toolbar() {
    const fresh = freshness(now(), props.dto()?.readAt ?? null, props.dto()?.error ?? null);
    return <View testID="dashboard-toolbar" style={{ flexDirection: "row", flexWrap: "wrap", alignItems: "center", gap: size(10), paddingHorizontal: size(DASHBOARD_METRICS.page), paddingVertical: size(6), borderBottomWidth: 1, borderColor: palette().divider }}>
      <View style={{ ...searchBox(props.theme, searchFocused()).style, flex: layout().arrangement === "compact" ? 1 : undefined, width: layout().arrangement === "compact" ? undefined : size(260) }} hoverStyle={searchBox(props.theme, searchFocused()).hoverStyle}>
        <Icon name="search" size={11 * scale()} color={palette().muted} />
        <Input testID="dashboard-search" accessibilityLabel="Search the dashboard" placeholder={touch() ? TOUCH_SEARCH_PLACEHOLDER : SEARCH_PLACEHOLDER} value={props.query()}
          ref={(node) => { search = node; }} onChange={({ value: text }) => props.query.set(text)} onFocus={() => searchFocused.set(true)} onBlur={() => searchFocused.set(false)}
          style={{ ...font(11), flex: 1, minWidth: 0, paddingVertical: 3, backgroundColor: "transparent" }} />
        {props.query() !== "" && button("dashboard-clear-search", "×", () => { props.query.set(""); search?.focus(); }, false, "Clear the search")}
      </View>
      {layout().arrangement !== "compact" && <Scroll horizontal={true} style={{ flex: 1, minWidth: 0 }}>{wire() && summaryStrip(wire()!)}</Scroll>}
      {fresh.warn ? <View testID="dashboard-freshness" style={{ flexShrink: 1, maxWidth: size(layout().arrangement === "compact" ? 140 : 280) }}><StatusPill theme={props.theme} tone="needs" label={fresh.text} /></View>
        : <Text testID="dashboard-freshness" style={{ ...font(10, palette().muted), flexShrink: 1, maxWidth: size(layout().arrangement === "compact" ? 105 : 260), numberOfLines: 1 }}>{fresh.text}</Text>}
      {button("dashboard-refresh", "↻", props.onRefresh, false, "Read everything again now")}
    </View>;
  }

  function summaryStrip(read: Wire) {
    const totals = read.totals;
    const copy = chatCopy(read.global, props.plan(), props.starting());
    const cell = (id: string, text: string, tone = palette().muted) =>
      <View testID={id} style={{ paddingVertical: size(4), flexShrink: 0 }}><Text style={font(11, tone)}>{text}</Text></View>;
    // What works and what waits are states, so they are pills; the counts beside them are words.
    const pill = (id: string, tone: Tone, text: string) => <View testID={id} style={{ paddingVertical: size(3), flexShrink: 0 }}><StatusPill theme={props.theme} tone={tone} label={text} /></View>;
    return <View testID="dashboard-summary" style={{ flexDirection: "row", flexWrap: layout().arrangement === "compact" ? "wrap" : "nowrap", alignItems: "center", gap: size(10), paddingHorizontal: layout().arrangement === "compact" ? size(DASHBOARD_METRICS.page) : 0 }}>
      {totals.agents.working > 0 ? pill("dashboard-working", "working", `${totals.agents.working} working${totals.agents.subagents ? ` · ${plural(totals.agents.subagents, "subagent")}` : ""}`) : cell("dashboard-working", "No agent working")}
      {totals.attention > 0 ? pill("dashboard-attention", "needs", `${totals.attention} need you`) : cell("dashboard-attention", "Nothing waits for you")}
      {cell("dashboard-live", `${plural(totals.live_shells, "live terminal")}${totals.live_chats != null ? ` · ${plural(totals.live_chats, "live chat")}` : ""}`, palette().text)}
      {cell("dashboard-tasks", totals.tasks.done + totals.tasks.todo + totals.tasks.in_progress ? `Tasks ${totals.tasks.done}/${totals.tasks.done + totals.tasks.todo + totals.tasks.in_progress} done` : "No tasks", palette().text)}
      <View testID="dashboard-global-chat-block" style={{ flexDirection: "row", alignItems: "center", gap: size(6), paddingVertical: size(4), flexShrink: 0 }}>
        <Text style={font(10, palette().muted)}>{copy.status}</Text>
        {button("dashboard-global-chat", copy.action, copy.enabled ? props.onGlobalChat : undefined, false, undefined, "outlined")}
      </View>
    </View>;
  }
  function controls(read: Wire) {
    const counts = filterCounts(read.projects ?? [], props.query());
    return <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6, alignItems: "center", paddingHorizontal: size(DASHBOARD_METRICS.page) }}>
      <Text style={{ ...font(10), fontWeight: "600" }}>Projects</Text>
      <Text style={{ ...font(10, palette().muted), fontVariant: ["tabular-nums"] }}>{`${visibleProjects(read.projects ?? [], props.query(), props.filter(), props.sort()).length} of ${read.projects_total ?? read.projects?.length ?? 0}`}</Text>
      <Segmented theme={props.theme} id="dashboard-filters" role="tab" accessibilityLabel="Filter projects" touch={touch()} stretch={stacked()}
        segments={FILTERS.map((filter) => ({ id: `dashboard-filter-${filter}`, label: filterLabel(filter), count: counts.find(([f]) => f === filter)?.[1] ?? 0, selected: props.filter() === filter,
          accessibilityLabel: `${props.filter() === filter ? "Showing" : "Show"} ${filterLabel(filter)}`, onPress: () => props.onFilter(filter) }))} />
      <Text style={font(10, palette().muted)}>Sort</Text>
      <Segmented theme={props.theme} id="dashboard-sorts" role="tab" accessibilityLabel="Order projects" touch={touch()} stretch={stacked()}
        segments={SORTS.map((sort) => ({ id: `dashboard-sort-${sort}`, label: sortLabel(sort), selected: props.sort() === sort,
          accessibilityLabel: `${props.sort() === sort ? "Ordered by" : "Order by"} ${sortLabel(sort)}`, onPress: () => props.onSort(sort) }))} />
    </View>;
  }
  function projectRow(project: ProjectRow, read: Wire) {
    const columns = COLUMN_WIDTHS[layout().columns];
    const state = projectState(project);
    const tone = { gold: palette().gold, working: palette().working, active: palette().muted, quiet: palette().divider }[state.tone];
    const tasks = tasksWords(project.tasks);
    const orchestrator = orchestratorWords(project);
    const where = [project.folder, columns.orchestrator == null && project.orchestrator ? `P·Orch ${orchestrator?.[0] ?? ""} ${orchestrator?.[1] ?? ""}`.trim() : undefined, project.worktrees ? plural(project.worktrees, "worktree") : undefined].filter(Boolean).join(" · ");
    const agents = agentParts(project);
    const toneOf = (part: string) => ({ gold: palette().gold, working: palette().working, cyan: palette().cyan, muted: palette().muted } as Record<string, string>)[part];
    if (stacked()) {
      // The same figures as the columns, under the name; the row opens the project.
      return <View testID={`dashboard-project-${project.id}`} role="button" accessibilityLabel={projectReaderName(project, read.chats_known, now())}
        onPress={() => props.onOpen({ type: "project", id: project.id })} focusable={true} activeStyle={{ backgroundColor: palette().panelActive }}
        style={{ flexDirection: "row", alignItems: "center", gap: size(DASHBOARD_METRICS.cellGap), padding: size(DASHBOARD_METRICS.rowPad), borderRadius: 7, minHeight: 44 }}>
        <Text style={{ ...font(11, tone), width: size(DASHBOARD_METRICS.mark) }}>{state.mark}</Text>
        <View style={{ flex: 1, minWidth: 0, gap: 2 }}>
          <Text style={{ ...font(12), numberOfLines: 1 }}>{project.name}</Text>
          {where && <Text style={{ ...font(10, palette().muted), numberOfLines: 1 }}>{where}</Text>}
          <Text style={{ ...font(10, palette().muted), numberOfLines: 1 }}>{agents.length ? agents.map((part, index) => <Text style={{ color: toneOf(part.tone) }}>{`${index ? " " : ""}${part.text}`}</Text>) : "No agents"}{` · ${sessionsLine(project, read.chats_known)}`}</Text>
          <Text style={{ ...font(10, palette().muted), numberOfLines: 2 }}>{`${tasks ? `${tasks[0]} · ${tasks[1]}` : "No tasks"} · ${age(now(), project.last_activity) ?? "—"}`}</Text>
          <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
            {button(`dashboard-tasks-${project.id}`, "Tasks", () => props.onPanel(project.id, "tasks"), false, undefined, "outlined")}
            {button(`dashboard-sessions-${project.id}`, "Sessions", () => props.onPanel(project.id, "shells"), false, undefined, "outlined")}
          </View>
        </View>
      </View>;
    }
    return <View testID={`dashboard-project-${project.id}`} role="button" accessibilityLabel={projectReaderName(project, read.chats_known, now())}
      onPress={() => props.onOpen({ type: "project", id: project.id })} focusable={true} hoverStyle={{ backgroundColor: palette().panelActive }}
      style={{ flexDirection: "row", alignItems: "center", gap: size(DASHBOARD_METRICS.cellGap), padding: size(DASHBOARD_METRICS.rowPad), borderRadius: 7 }}>
      <Text style={{ ...font(11, tone), width: size(DASHBOARD_METRICS.mark) }}>{state.mark}</Text>
      <View style={{ flex: 1, minWidth: size(DASHBOARD_METRICS.projectMin), gap: 2 }}>
        <Text style={{ ...font(12), numberOfLines: 1 }}>{project.name}</Text>
        {where && <Text style={{ ...font(10, palette().muted), numberOfLines: 1 }}>{where}</Text>}
        {columns.activity == null && <Text style={{ ...font(10, palette().muted), numberOfLines: 1 }}>{`${agents.map((part) => part.text).join(" ") || "No agents"} · ${sessionsLine(project, read.chats_known)}`}</Text>}
      </View>
      {columns.activity != null && <View style={{ width: size(columns.activity!), gap: 2 }}>
        <Text style={{ ...font(10, palette().muted), numberOfLines: 1 }}>{agents.length ? agents.map((part, index) => <Text style={{ color: toneOf(part.tone) }}>{`${index ? " " : ""}${part.text}`}</Text>) : "No agents"}</Text>
        <Text style={{ ...font(10, palette().muted), numberOfLines: 1 }}>{sessionsLine(project, read.chats_known)}</Text>
      </View>}
      <View style={{ width: size(columns.tasks), gap: 2 }}>
        <Text style={font(10)}>{tasks?.[0] ?? "No tasks"}</Text>
        {tasks && <Text style={font(10, palette().muted)}>{tasks[1]}</Text>}
      </View>
      {columns.orchestrator != null && <Text style={{ ...font(10, palette().muted), width: size(columns.orchestrator!), numberOfLines: 1 }}>{orchestrator ? `${orchestrator[0]} ${orchestrator[1]}` : "None"}</Text>}
      <Text style={{ ...font(10, palette().muted), width: size(columns.last), numberOfLines: 1 }}>{age(now(), project.last_activity) ?? "—"}</Text>
      <View style={{ width: size(columns.actions), flexDirection: "row", gap: 4 }}>
        {button(`dashboard-project-tasks-${project.id}`, "Tasks", () => props.onPanel(project.id, "tasks"), false, undefined, "outlined")}
        {button(`dashboard-project-sessions-${project.id}`, "Sessions", () => props.onPanel(project.id, "shells"), false, undefined, "outlined")}
      </View>
    </View>;
  }
  function tableHead() {
    const columns = COLUMN_WIDTHS[layout().columns];
    const label = (title: string, width: number) => <Text style={{ ...font(9, palette().muted), fontWeight: "600", width: size(width), numberOfLines: 1 }}>{title}</Text>;
    return <View testID="dashboard-table-head" style={{ flexDirection: "row", alignItems: "center", gap: size(DASHBOARD_METRICS.cellGap), paddingHorizontal: size(DASHBOARD_METRICS.rowPad), paddingVertical: size(3), borderBottomWidth: 1, borderColor: palette().divider }}>
      <View style={{ width: size(DASHBOARD_METRICS.mark) }} />
      <Text style={{ ...font(9, palette().muted), fontWeight: "600", flex: 1, minWidth: size(DASHBOARD_METRICS.projectMin) }}>Project</Text>
      {columns.activity != null && label("Activity", columns.activity)}
      {label("Tasks", columns.tasks)}
      {columns.orchestrator != null && label("Orchestrator", columns.orchestrator)}
      {label("Last active", columns.last)}
      <View style={{ width: size(columns.actions) }} />
    </View>;
  }
  function table(read: Wire) {
    const shown = visibleProjects(read.projects ?? [], props.query(), props.filter(), props.sort());
    const empty = shown.length ? null : emptyTable(read.projects ?? [], props.query(), props.filter());
    const contents = () => <>
      {shown.map((project) => projectRow(project, read))}
      {more(read.projects_total, read.projects?.length ?? 0)}
      {empty && <View testID="dashboard-empty" style={{ padding: 16, gap: 8, alignItems: "flex-start" }}>
        <Text style={font(11, palette().muted)}>{empty.text}</Text>
        {empty.showAll && button("dashboard-show-all", empty.showAll, () => props.onFilter("all"))}
      </View>}
    </>;
    return <View style={{ minHeight: 0 }}>
      {!stacked() && tableHead()}
      {shown.length > 8 ? <Scroll testID="dashboard-table" style={{ height: size(480), minHeight: 0 }}>{contents()}</Scroll>
        : <View testID="dashboard-table">{contents()}</View>}
    </View>;
  }
  /** Rows are kept by request: a new read redraws a row in place, so the review
   * unfolded in it keeps its answers and the keyboard focus. The unfolded one
   * stays listed where it was after its request leaves the list, until folded. */
  function decisions() {
    let at = 0;
    const found = () => (wire()?.decisions ?? []).filter((decision) => matches(props.query(), [...sessionFields(decision.session), decision.kind]));
    const rows = () => {
      const list = found(), open = review()?.decision;
      const index = open ? list.findIndex((decision) => decision.id === open.id) : -1;
      if (!open || index >= 0) { at = Math.max(0, index); return list; }
      return [...list.slice(0, at), open, ...list.slice(at)];
    };
    const sources = () => wire()?.decision_sources;
    return section("dashboard-decisions", `Decisions · ${found().length}`, <>
      <Text testID="dashboard-decision-sources" style={{ ...font(10, palette().muted), paddingHorizontal: size(8) }}>{!sources() || sources()!.state === "unavailable" ? "Chats not read yet · requests unknown"
        : sources()!.state === "partial" ? `${sources()!.chats_complete} of ${plural(sources()!.chats_total, "chat")} checked · some requests may be missing`
          : `${plural(sources()!.chats_total, "chat")} checked`}</Text>
      <Each of={rows()} key={(decision) => decision.id}>{(decision) => decisionRow(decision)}</Each>
      {more(props.query().trim() ? found().length : wire()?.decisions_total ?? found().length, found().length)}
      {!rows().length && <Text style={note()}>{wire()?.decisions?.length && props.query().trim() ? `Nothing here matches “${props.query().trim()}”.`
        : sources()?.state === "read" ? "No pending approvals or questions." : "No requests found in the available events. Open Needs you sessions for more."}</Text>}
    </>);
  }
  function decisionRow(decision: () => DecisionRow) {
    const id = decision().id, open = computed(() => review()?.decision.id === id);
    let opener: ViewRef | undefined;
    // A decision is a card of its own; its Review is the dashboard's one filled action.
    const action = () => buttonLook(props.theme, open() ? "default" : "primary", !!review()?.busy());
    return <View testID={`dashboard-decision-row-${id}`} style={{ borderRadius: size(14), borderWidth: 1, borderColor: open() ? palette().text : palette().divider, marginBottom: 8, backgroundColor: palette().bg }}>
      <Button testID={`dashboard-decision-${id}`} selected={open()} disabled={!!review()?.busy()} ref={(node) => { opener = node; }} onPress={() => unfold(decision())}
        accessibilityLabel={`${open() ? "Hide the review of" : "Review"} ${sessionName(decision().session)}'s ${decision().kind === "approval" ? "approval request" : "question"}`}
        style={{ flexDirection: "row", alignItems: "center", gap: size(10), paddingHorizontal: size(12), paddingVertical: size(10), borderRadius: size(13) }}
        hoverStyle={{ backgroundColor: palette().panel }} focusStyle={focusRing(palette(), -2)}>
        <View style={{ flex: 1, minWidth: 0, gap: size(3) }}>
          <Text style={{ ...font(12), fontWeight: "600", numberOfLines: 3 }}>{`${sessionName(decision().session)} asks ${decision().kind === "approval" ? "for approval" : "a question"}`}</Text>
          <Text style={{ ...font(10, palette().muted), numberOfLines: 1 }}>{`${decision().session.project_name ?? "Global"} · ${age(now(), decision().observed_at) ?? ""}`}</Text>
        </View>
        <View style={{ ...action().style, minHeight: size(touch() ? 44 : 28), paddingHorizontal: size(14) }}>
          <Text style={action().label}>{open() ? "Hide" : "Review"}</Text>
        </View>
      </Button>
      {open() && <DecisionReviewView review={review()!} theme={props.theme} opener={() => opener} onClose={fold}
        onOpenChat={() => { const target = decision().session.target; fold(); props.onOpen(target); }} />}
    </View>;
  }
  function urgentActivity(read: Wire) {
    // Requests with a decision card already have an explicit review action above.
    const attention = (read.attention ?? []).filter((item) => !read.decisions?.some((decision) => item.kind === "waiting" && item.target.type === "chat" && item.target.id === decision.chat_id));
    const total = Math.max(attention.length, (read.attention_total ?? read.attention?.length ?? 0) - ((read.attention?.length ?? 0) - attention.length));
    return <>
      {section("dashboard-attention-list", `Needs you · ${total}`, list(attention, total, (item, index) => {
        const [headline, detail] = attentionCopy(item, now());
        const failed = item.kind === "chat_failed" || item.kind === "automation" && item.outcome !== "missed" && item.outcome !== "uncertain";
        return row(`dashboard-attention-${index}`, headline, detail, item.target, palette().text, { tone: failed ? "failed" : "needs", label: failed ? "Failed" : "Waiting" });
      }, quietAttention(read), (item) => [...attentionCopy(item, now()), item.project_name ?? "", item.session?.project_name ?? ""], Number.POSITIVE_INFINITY))}
      {section("dashboard-working-list", `Working now · ${read.working_total ?? read.working?.length ?? 0}`, list(read.working, read.working_total, (work, index) =>
        row(`dashboard-working-${index}`, sessionName(work.session), workDetail(work, now(), false), work.session.target), "No agent is working right now.", (work) => sessionFields(work.session)))}
    </>;
  }
  /** The rest of the activity rail, drawn again on each read. */
  function activity(read: Wire) {
    const intelligence = intelligenceSection(read.intelligence, now());
    // What needs the person comes first, under the decisions; the alert colours are
    // theirs alone (the section title says the rest are working): red for what
    // failed, the attention colour for what waits or was missed.
    return <>
      {intelligence && section("dashboard-intelligence", "Overview", <View style={{ gap: 3, paddingHorizontal: size(8) }}>
        {intelligence.summary && <Text style={font(11)}>{intelligence.summary}</Text>}
        {intelligence.highlights.map((text) => <Text style={font(10, palette().muted)}>{text}</Text>)}
        <Text testID="dashboard-intelligence-status" style={font(10, palette().muted)}>{intelligence.status}</Text>
        {intelligence.error && <Text style={font(10, palette().gold)}>{intelligence.error}</Text>}
      </View>, "Haiku")}
      {!!read.recent_activity?.length && section("dashboard-recent-events", "Recent activity", list(read.recent_activity, read.recent_activity_total, (item, index) =>
        row(`dashboard-recent-event-${index}`, item.text, [sessionName(item.session), item.status.replaceAll("_", " "), age(now(), item.log_updated_at)].filter(Boolean).join(" · "), item.session.target),
        "No recent activity.", (item) => [item.text, ...sessionFields(item.session)], 6))}
      {section("dashboard-finished-list", "Finished in the last hour", list(read.finished, read.finished_total, (work, index) =>
        row(`dashboard-finished-${index}`, sessionName(work.session), workDetail(work, now(), true), work.session.target), "No agent finished in the last hour.", (work) => sessionFields(work.session)))}
    </>;
  }
  function workRail(read: Wire) {
    const automations = read.automations;
    const allLines = props.dto()?.backendLines ?? null;
    const lines = allLines?.filter((line) => matches(props.query(), [line.title, line.detail])) ?? null;
    const mark = { current: "●", stale: "↻", unknown: "?", idle: "○" } as const;
    return <>
      {section("dashboard-tasks-list", "Tasks in progress", list(read.tasks_in_progress, read.tasks_in_progress_total, (task) =>
        row(`dashboard-task-${task.id}`, task.title, [task.project_name, task.branch, age(now(), task.updated_at)].filter(Boolean).join(" · "), { type: "task", id: task.id }), "No task is in progress.", (task) => [task.title, task.project_name, task.branch ?? ""]))}
      {section("dashboard-automations", "Automations", automations.state === "not_read" ? <Text style={note()}>Reading automations…</Text>
        : automations.state === "unreadable" ? <Text style={note(palette().danger)}>{`Automations could not be read: ${automations.message}`}</Text>
          : list(automations.items, automations.total, (item) => row(`dashboard-automation-${item.id}`, item.title, automationDetail(item, now()),
            { type: "automations", project_id: item.project_id }), "No automations.", (item) => [item.title, item.project_name ?? "App"]))}
      {section("dashboard-backends", "Backends", lines == null ? <Text style={note()}>Reading the app, the chat host and the providers…</Text>
        : lines.length === 0 ? <Text style={note()}>{allLines?.length ? `Nothing here matches “${props.query().trim()}”.` : "No provider CLI was found."}</Text>
          : <>{lines.map((line) => <View testID={`dashboard-backend-${line.key}`} accessibilityLabel={`${line.title}; ${line.detail}`} style={{ gap: 2, paddingHorizontal: size(8), paddingVertical: size(3) }}>
            <Text style={{ ...font(11, line.tone === "stale" ? palette().gold : line.tone === "current" ? palette().cyan : palette().muted), numberOfLines: 1 }}>{`${mark[line.tone]} ${line.title}`}</Text>
            <Text style={{ ...font(10, palette().muted), numberOfLines: 1 }}>{line.detail}</Text>
          </View>)}</>)}
    </>;
  }
  /** Priority stays in the same mounted tree across window sizes so review drafts survive. */
  function body() {
    const columns = () => (width() || windowSize().width) / Math.max(scale(), 1) >= 700 ? 2 : 1;
    return <Scroll testID="dashboard-content" style={{ flex: 1, minHeight: 0 }} contentStyle={{ paddingHorizontal: size(12), paddingBottom: size(16), gap: size(18) }}>
      <View testID="dashboard-priority" style={{ display: "grid", gridTemplateColumns: columns() > 1 ? "minmax(0, 1fr) minmax(0, 1fr)" : "minmax(0, 1fr)", gap: size(16), alignItems: "start" }}>
        <View style={{ minWidth: 0 }}>{decisions()}</View>
        <View style={{ minWidth: 0 }}>{urgentActivity(wire()!)}</View>
      </View>
      <View testID="dashboard-projects-region" style={{ minWidth: 0, gap: size(8), paddingVertical: size(8) }}>
        {controls(wire()!)}{table(wire()!)}
      </View>
      <View testID="dashboard-secondary" style={{ display: "grid", gridTemplateColumns: columns() > 1 ? "minmax(0, 1fr) minmax(0, 1fr)" : "minmax(0, 1fr)", gap: size(16), alignItems: "start" }}>
        <View style={{ minWidth: 0 }}>{activity(wire()!)}</View>
        <View style={{ minWidth: 0 }}>{workRail(wire()!)}</View>
      </View>
    </Scroll>;
  }
  return <View testID="dashboard" onLayout={({ bounds }) => { if (bounds.size.width !== width()) width.set(bounds.size.width); }} style={{ flex: 1, minHeight: 0, gap: size(8), paddingBottom: size(8), backgroundColor: palette().panel }}>
    {toolbar()}
    <If when={!!wire()} else={<View testID="dashboard-unread" style={{ flex: 1, alignItems: "center", justifyContent: "center", gap: 8, padding: 24 }}>
      <Text style={font(18, palette().muted)}>{props.dto()?.error ? "⚠" : "⧗"}</Text>
      <Text style={{ ...font(11, palette().muted), textAlign: "center" }}>{props.dto()?.error
        ? "The sessions could not be read, so nothing can be counted yet. Somtus tries again every two seconds; Refresh tries now."
        : "Reading the sessions, chats and tasks of every project…"}</Text>
    </View>}>
      <View style={{ flex: 1, minHeight: 0, gap: size(8) }}>
        {arrangement() === "compact" && summaryStrip(wire()!)}
        {body()}
      </View>
    </If>
  </View>;
}
