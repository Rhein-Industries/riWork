import { Scroll, Text, View, signal, windowSize, type JSX, type YNode } from "nsh";
import { Input } from "nsh/base";
import type { DashboardEvent } from "riwork/native";
import { type InterfaceTheme } from "../appearance";
import {
  COLUMN_WIDTHS, DASHBOARD_METRICS, FILTERS, LIST_LIMIT, SORTS, age, agentParts, attentionCopy, automationDetail,
  chatCopy, emptyTable, filterCounts, filterLabel, freshness, intelligenceSection, layoutFor, orchestratorWords,
  plural, projectReaderName, projectState, quietAttention, sessionName, sessionsLine, sortLabel, tasksWords,
  visibleProjects, workDetail, type Filter, type Plan, type ProjectRow, type Sort, type Target, type Wire,
} from "./model";

type Dto = DashboardEvent["data"];
type Region = "projects" | "activity" | "work";

export const SEARCH_PLACEHOLDER = "Search projects, tasks and sessions  ⌘F";

/** The global dashboard (PANEL-078..099). It draws what the native read says and
 * asks for nothing while drawing: every click is an explicit route or refresh. */
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
}) {
  // The compact region is not saved; the search is never saved.
  const region = signal<Region>("projects");
  const palette = () => props.theme().palette;
  const scale = () => props.theme().scale;
  const font = (size: number, color = palette().text) => ({ fontFamily: props.theme().family, fontSize: size * scale(), color });
  const mono = (size: number, color = palette().text) => ({ fontFamily: props.theme().monoFamily, fontSize: size * scale(), color });
  const wire = () => (props.dto()?.wire ?? null) as Wire | null;
  const now = () => props.dto()?.now ?? 0;
  const layout = () => layoutFor(windowSize().width, scale());
  let search: YNode | undefined;
  props.onSearch({ focus: () => search?.focus() });

  function button(id: string, label: string, onPress: (() => void) | undefined, selected = false, hint?: string) {
    return <View testID={id} role="button" accessibilityLabel={hint ?? label} accessibilityState={{ selected }} disabled={!onPress} onPress={onPress}
      style={{ paddingHorizontal: 8, paddingVertical: 3, borderRadius: 5, borderWidth: 1, borderColor: selected ? palette().cyan : palette().divider, backgroundColor: selected ? palette().panelActive : "transparent" }}>
      <Text numberOfLines={1} style={font(10, selected ? palette().cyan : palette().text)}>{label}</Text>
    </View>;
  }
  function section(id: string, title: string, children: JSX.Element | JSX.Element[], meta?: string) {
    return <View testID={id} style={{ gap: 6, padding: 10, borderWidth: 1, borderColor: palette().divider, borderRadius: 8, backgroundColor: palette().panel }}>
      <View style={{ flexDirection: "row", justifyContent: "space-between" }}>
        <Text style={mono(10, palette().muted)}>{title.toUpperCase()}</Text>
        {meta && <Text style={mono(10, palette().muted)}>{meta}</Text>}
      </View>
      {children}
    </View>;
  }
  function row(id: string, headline: string, detail: string, target?: Target, tone = palette().text) {
    return <View testID={id} role={target ? "button" : "text"} accessibilityLabel={`${headline}; ${detail}`} onPress={target ? () => props.onOpen(target) : undefined} style={{ gap: 2 }}>
      <Text numberOfLines={1} style={font(11, tone)}>{headline}</Text>
      <Text numberOfLines={1} style={font(10, palette().muted)}>{detail}</Text>
    </View>;
  }
  function more(total: number | undefined, shown: number) {
    return total != null && total > shown ? <Text style={font(10, palette().muted)}>{`${total - shown} more`}</Text> : null;
  }
  function list<T>(rows: T[] | undefined, total: number | undefined, draw: (item: T, index: number) => JSX.Element, empty: string) {
    const items = (rows ?? []).slice(0, LIST_LIMIT);
    return items.length ? <>{items.map(draw)}{more(total ?? rows?.length, items.length)}</> : <Text style={font(10, palette().muted)}>{empty}</Text>;
  }

  // ---- Regions ---------------------------------------------------------------------------
  function toolbar() {
    const fresh = freshness(now(), props.dto()?.readAt ?? null, props.dto()?.error ?? null);
    return <View testID="dashboard-toolbar" style={{ flexDirection: "row", alignItems: "center", gap: 8, paddingHorizontal: DASHBOARD_METRICS.page, paddingVertical: 6 }}>
      <Text style={font(13, palette().muted)}>⌕</Text>
      <Input testID="dashboard-search" accessibilityLabel="Search the dashboard" placeholder={SEARCH_PLACEHOLDER} value={props.query()}
        ref={(node) => { search = node; }} onChange={(text) => props.query.set(text)}
        style={{ ...font(11), flex: 1, minWidth: 120, paddingHorizontal: 6, paddingVertical: 3, borderWidth: 1, borderColor: palette().divider, borderRadius: 5 }} />
      {props.query() !== "" && button("dashboard-clear-search", "×", () => { props.query.set(""); search?.focus(); }, false, "Clear the search")}
      <Text testID="dashboard-freshness" numberOfLines={1} style={font(10, fresh.warn ? palette().gold : palette().muted)}>{fresh.text}</Text>
      {button("dashboard-refresh", "↻", props.onRefresh, false, "Read everything again now")}
    </View>;
  }
  function summaryStrip(read: Wire) {
    const totals = read.totals;
    const copy = chatCopy(read.global, props.plan(), props.starting());
    const cell = (id: string, text: string, tone = palette().text) =>
      <View testID={id} style={{ paddingHorizontal: 10, paddingVertical: 6, borderRadius: 6, borderWidth: 1, borderColor: palette().divider }}><Text style={font(11, tone)}>{text}</Text></View>;
    return <View testID="dashboard-summary" style={{ flexDirection: "row", flexWrap: "wrap", gap: 8, paddingHorizontal: DASHBOARD_METRICS.page }}>
      {cell("dashboard-working", totals.agents.working > 0 ? `${totals.agents.working} working${totals.agents.subagents ? ` · ${plural(totals.agents.subagents, "subagent")}` : ""}` : "No agent working", totals.agents.working ? palette().working : palette().muted)}
      {cell("dashboard-attention", totals.attention > 0 ? `${totals.attention} need you` : "Nothing waits for you", totals.attention ? palette().gold : palette().muted)}
      {cell("dashboard-live", `${plural(totals.live_shells, "live terminal")}${totals.live_chats != null ? ` · ${plural(totals.live_chats, "live chat")}` : ""}`)}
      {cell("dashboard-tasks", totals.tasks.done + totals.tasks.todo + totals.tasks.in_progress ? `Tasks ${totals.tasks.done}/${totals.tasks.done + totals.tasks.todo + totals.tasks.in_progress} done` : "No tasks")}
      <View testID="dashboard-global-chat-block" style={{ flexDirection: "row", alignItems: "center", gap: 8, paddingHorizontal: 10, paddingVertical: 4, borderRadius: 6, borderWidth: 1, borderColor: palette().magenta }}>
        <Text style={font(10, palette().muted)}>{copy.status}</Text>
        {button("dashboard-global-chat", copy.action, copy.enabled ? props.onGlobalChat : undefined)}
      </View>
    </View>;
  }
  function controls(read: Wire) {
    const counts = filterCounts(read.projects ?? [], props.query());
    return <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6, alignItems: "center", paddingHorizontal: DASHBOARD_METRICS.page }}>
      <View testID="dashboard-filters" role="tablist" accessibilityLabel="Filter projects" style={{ flexDirection: "row", gap: 4 }}>
        {FILTERS.map((filter) => button(`dashboard-filter-${filter}`, `${filterLabel(filter)} ${counts.find(([f]) => f === filter)?.[1] ?? 0}`, () => props.onFilter(filter), props.filter() === filter,
          `${props.filter() === filter ? "Showing" : "Show"} ${filterLabel(filter)}`))}
      </View>
      <Text style={font(10, palette().muted)}>Order</Text>
      <View testID="dashboard-sorts" role="tablist" accessibilityLabel="Order projects" style={{ flexDirection: "row", gap: 4 }}>
        {SORTS.map((sort) => button(`dashboard-sort-${sort}`, sortLabel(sort), () => props.onSort(sort), props.sort() === sort,
          `${props.sort() === sort ? "Ordered by" : "Order by"} ${sortLabel(sort)}`))}
      </View>
    </View>;
  }
  function projectRow(project: ProjectRow, read: Wire) {
    const columns = COLUMN_WIDTHS[layout().columns];
    const state = projectState(project);
    const tone = { gold: palette().gold, working: palette().working, active: palette().muted, quiet: palette().divider }[state.tone];
    const tasks = tasksWords(project.tasks);
    const orchestrator = orchestratorWords(project);
    const where = [project.folder, project.orchestrator ? `P·Orch ${orchestrator?.[0] ?? ""} ${orchestrator?.[1] ?? ""}`.trim() : undefined, project.worktrees ? plural(project.worktrees, "worktree") : undefined].filter(Boolean).join(" · ");
    const agents = agentParts(project);
    const toneOf = (part: string) => ({ gold: palette().gold, working: palette().working, cyan: palette().cyan, muted: palette().muted } as Record<string, string>)[part];
    return <View testID={`dashboard-project-${project.id}`} role="button" accessibilityLabel={projectReaderName(project, read.chats_known, now())}
      onPress={() => props.onOpen({ type: "project", id: project.id })}
      style={{ flexDirection: "row", alignItems: "center", gap: DASHBOARD_METRICS.cellGap, padding: DASHBOARD_METRICS.rowPad, borderBottomWidth: 1, borderColor: palette().divider }}>
      <Text style={{ ...font(11, tone), width: DASHBOARD_METRICS.mark }}>{state.mark}</Text>
      <View style={{ flex: 1, minWidth: DASHBOARD_METRICS.projectMin, gap: 2 }}>
        <Text numberOfLines={1} style={font(12)}>{project.name}</Text>
        {where && <Text numberOfLines={1} style={font(10, palette().muted)}>{where}</Text>}
        {columns.activity == null && <Text numberOfLines={1} style={font(10, palette().muted)}>{`${agents.map((part) => part.text).join(" ") || "No agents"} · ${sessionsLine(project, read.chats_known)}`}</Text>}
      </View>
      {columns.activity != null && <View style={{ width: columns.activity, gap: 2 }}>
        <Text numberOfLines={1} style={font(10, agents.length ? toneOf(agents[0].tone) : palette().muted)}>{agents.map((part) => part.text).join(" ") || "No agents"}</Text>
        <Text numberOfLines={1} style={font(10, palette().muted)}>{sessionsLine(project, read.chats_known)}</Text>
      </View>}
      <View style={{ width: columns.tasks, gap: 2 }}>
        <Text style={font(10)}>{tasks?.[0] ?? "No tasks"}</Text>
        {tasks && <Text style={font(10, palette().muted)}>{tasks[1]}</Text>}
      </View>
      {columns.orchestrator != null && <Text numberOfLines={1} style={{ ...font(10, palette().muted), width: columns.orchestrator }}>{orchestrator ? `${orchestrator[0]} ${orchestrator[1]}` : "None"}</Text>}
      <Text numberOfLines={1} style={{ ...font(10, palette().muted), width: columns.last }}>{age(now(), project.last_activity) ?? "—"}</Text>
      <View style={{ width: columns.actions, flexDirection: "row", gap: 4 }}>
        {button(`dashboard-project-tasks-${project.id}`, "Tasks", () => props.onPanel(project.id, "tasks"))}
        {button(`dashboard-project-sessions-${project.id}`, "Sessions", () => props.onPanel(project.id, "shells"))}
      </View>
    </View>;
  }
  function table(read: Wire) {
    const shown = visibleProjects(read.projects ?? [], props.query(), props.filter(), props.sort());
    const empty = shown.length ? null : emptyTable(read.projects ?? [], props.query(), props.filter());
    return <Scroll testID="dashboard-table" style={{ flex: 1, minHeight: 0 }} contentStyle={{ paddingHorizontal: DASHBOARD_METRICS.page }}>
      {shown.map((project) => projectRow(project, read))}
      {more(read.projects_total, read.projects?.length ?? 0)}
      {empty && <View testID="dashboard-empty" style={{ padding: 16, gap: 8, alignItems: "flex-start" }}>
        <Text style={font(11, palette().muted)}>{empty.text}</Text>
        {empty.showAll && button("dashboard-show-all", empty.showAll, () => props.onFilter("all"))}
      </View>}
    </Scroll>;
  }
  function activityRail(read: Wire) {
    const intelligence = intelligenceSection(read.intelligence, now());
    const sources = read.decision_sources;
    return <>
      {section("dashboard-decisions", "Decisions", <>
        <Text style={font(10, palette().muted)}>{!sources || sources.state === "unavailable" ? "Chats could not be listed · requests unknown"
          : sources.state === "partial" ? `Partial coverage · ${sources.chats_complete} of ${sources.chats_total} chat logs complete` : `Checked ${plural(sources.chats_total, "chat")} · provider requests`}</Text>
        {list(read.decisions, read.decisions_total, (decision) => row(`dashboard-decision-${decision.id}`,
          `${sessionName(decision.session)} asks ${decision.kind === "approval" ? "for approval" : "a question"}`,
          `${decision.session.project_name ?? "Global"} · ${age(now(), decision.observed_at) ?? ""}`, decision.session.target),
        sources?.state === "read" ? "No pending approvals or questions." : "No requests found in the available events. Open Needs you sessions for more.")}
      </>)}
      {intelligence && section("dashboard-intelligence", "Overview", <>
        {intelligence.summary && <Text style={font(11)}>{intelligence.summary}</Text>}
        {intelligence.highlights.map((text) => <Text style={font(10, palette().muted)}>{text}</Text>)}
        <Text testID="dashboard-intelligence-status" style={font(10, palette().muted)}>{intelligence.status}</Text>
        {intelligence.error && <Text style={font(10, palette().gold)}>{intelligence.error}</Text>}
      </>, "Haiku")}
      {section("dashboard-attention-list", "Needs you", list(read.attention, read.attention_total, (item, index) => {
        const [headline, detail] = attentionCopy(item, now());
        return row(`dashboard-attention-${index}`, headline, detail, item.target, palette().gold);
      }, quietAttention(read)))}
      {section("dashboard-working-list", "Working now", list(read.working, read.working_total, (work, index) =>
        row(`dashboard-working-${index}`, sessionName(work.session), workDetail(work, now(), false), work.session.target, palette().working), "No agent is working right now."))}
      {section("dashboard-finished-list", "Finished in the last hour", list(read.finished, read.finished_total, (work, index) =>
        row(`dashboard-finished-${index}`, sessionName(work.session), workDetail(work, now(), true), work.session.target), "No agent finished in the last hour."))}
    </>;
  }
  function workRail(read: Wire) {
    const automations = read.automations;
    const lines = props.dto()?.backendLines ?? null;
    const mark = { current: "●", stale: "↻", unknown: "?", idle: "○" } as const;
    return <>
      {section("dashboard-tasks-list", "Tasks in progress", list(read.tasks_in_progress, read.tasks_in_progress_total, (task) =>
        row(`dashboard-task-${task.id}`, task.title, [task.project_name, task.branch, age(now(), task.updated_at)].filter(Boolean).join(" · "), { type: "task", id: task.id }), "No task is in progress."))}
      {section("dashboard-automations", "Automations", automations.state === "not_read" ? <Text style={font(10, palette().muted)}>Reading automations…</Text>
        : automations.state === "unreadable" ? <Text style={font(10, palette().gold)}>{`Automations could not be read: ${automations.message}`}</Text>
          : list(automations.items, automations.total, (item) => row(`dashboard-automation-${item.id}`, item.title, automationDetail(item, now()),
            { type: "automations", project_id: item.project_id }), "No automations."))}
      {section("dashboard-backends", "Backends", lines == null ? <Text style={font(10, palette().muted)}>Reading the app, the chat host and the providers…</Text>
        : lines.length === 0 ? <Text style={font(10, palette().muted)}>No provider CLI was found.</Text>
          : <>{lines.map((line) => <View testID={`dashboard-backend-${line.key}`} accessibilityLabel={`${line.title}; ${line.detail}`} style={{ gap: 2 }}>
            <Text numberOfLines={1} style={font(11, line.tone === "stale" ? palette().gold : line.tone === "current" ? palette().cyan : palette().muted)}>{`${mark[line.tone]} ${line.title}`}</Text>
            <Text numberOfLines={1} style={font(10, palette().muted)}>{line.detail}</Text>
          </View>)}</>)}
    </>;
  }
  function body(read: Wire) {
    const arrangement = layout().arrangement;
    const rail = (children: JSX.Element) => <Scroll style={{ width: DASHBOARD_METRICS.rail * Math.max(scale(), 1), minHeight: 0 }} contentStyle={{ gap: DASHBOARD_METRICS.gap, paddingBottom: 10 }}>{children}</Scroll>;
    if (arrangement === "compact") {
      const activity = (read.attention_total ?? read.attention?.length ?? 0);
      return <View style={{ flex: 1, minHeight: 0, gap: 8 }}>
        <View testID="dashboard-regions" role="tablist" style={{ flexDirection: "row", gap: 4, paddingHorizontal: DASHBOARD_METRICS.page }}>
          {button("dashboard-region-projects", `Projects ${read.projects_total ?? read.projects?.length ?? 0}`, () => region.set("projects"), region() === "projects")}
          {button("dashboard-region-activity", activity ? `Activity · ${activity} need you` : `Activity ${(read.working?.length ?? 0) + (read.finished?.length ?? 0)}`, () => region.set("activity"), region() === "activity")}
          {button("dashboard-region-work", `Tasks ${read.tasks_in_progress_total ?? read.tasks_in_progress?.length ?? 0}`, () => region.set("work"), region() === "work")}
        </View>
        {region() === "projects" ? <View style={{ flex: 1, minHeight: 0, gap: 8 }}>{controls(read)}{table(read)}</View>
          : <Scroll style={{ flex: 1, minHeight: 0 }} contentStyle={{ gap: DASHBOARD_METRICS.gap, paddingHorizontal: DASHBOARD_METRICS.page }}>
            {region() === "activity" ? activityRail(read) : workRail(read)}
          </Scroll>}
      </View>;
    }
    return <View style={{ flex: 1, minHeight: 0, flexDirection: "row", gap: DASHBOARD_METRICS.gap, paddingRight: DASHBOARD_METRICS.page }}>
      <View style={{ flex: 1, minHeight: 0, gap: 8 }}>{controls(read)}{table(read)}</View>
      {arrangement === "wide" ? <>{rail(activityRail(read))}{rail(workRail(read))}</> : rail(<>{activityRail(read)}{workRail(read)}</>)}
    </View>;
  }
  return <View testID="dashboard" style={{ flex: 1, minHeight: 0, gap: 8, paddingBottom: 8 }}>
    {toolbar()}
    {wire() ? <>{summaryStrip(wire()!)}{body(wire()!)}</> :
      <View testID="dashboard-unread" style={{ flex: 1, alignItems: "center", justifyContent: "center", gap: 8, padding: 24 }}>
        <Text style={font(18, palette().muted)}>{props.dto()?.error ? "⚠" : "⧗"}</Text>
        <Text style={{ ...font(11, palette().muted), textAlign: "center" }}>{props.dto()?.error
          ? "The sessions could not be read, so nothing can be counted yet. RiWork tries again every two seconds; Refresh tries now."
          : "Reading the sessions, chats and tasks of every project…"}</Text>
      </View>}
  </View>;
}
