import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";

import type { AgentStep } from "@/api";
import AgentJournal from "../AgentJournal.vue";

const steps: AgentStep[] = [
  { seq: 1, kind: "search", detail: "rust", reason: "Start with the goal", new_hits: 4 },
  { seq: 2, kind: "search", detail: "rust latest", reason: "Refine for recency", new_hits: 0 },
  { seq: 3, kind: "finish", detail: "", reason: "Coverage looks sufficient", new_hits: 0 },
];

describe("AgentJournal", () => {
  it("renders one entry per decision with query, hit count and reason", () => {
    const wrapper = mount(AgentJournal, { props: { steps, live: false } });
    const entries = wrapper.findAll("li");
    expect(entries).toHaveLength(3);
    expect(entries[0].text()).toContain("“rust”");
    expect(entries[0].text()).toContain("4 new results");
    expect(entries[1].text()).toContain("0 new results");
    expect(entries[1].text()).toContain("Refine for recency");
    expect(entries[2].text()).toContain("finished");
    expect(entries[2].text()).toContain("Coverage looks sufficient");
  });

  it("shows a thinking indicator while the job is live", () => {
    const wrapper = mount(AgentJournal, { props: { steps: steps.slice(0, 1), live: true } });
    expect(wrapper.find("[data-testid='agent-thinking']").exists()).toBe(true);
  });

  it("hides the indicator and shows an empty notice when idle without steps", () => {
    const wrapper = mount(AgentJournal, { props: { steps: [], live: false } });
    expect(wrapper.find("[data-testid='agent-thinking']").exists()).toBe(false);
    expect(wrapper.text()).toContain("No decisions recorded.");
  });
});
