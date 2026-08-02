import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { useAuthStore } from "@/stores/auth";
import ProfileView from "../ProfileView.vue";
import { makePinia, makeRouter } from "./helpers";

vi.mock("@/api", async (importOriginal) => {
  const original = await importOriginal<typeof import("@/api")>();
  return {
    ...original,
    api: { getProfile: vi.fn(), addChannel: vi.fn(), deleteChannel: vi.fn() },
  };
});

const { api, ApiError } = await import("@/api");
const mocked = api as unknown as Record<
  "getProfile" | "addChannel" | "deleteChannel",
  ReturnType<typeof vi.fn>
>;

const emptyProfile = {
  email: "me@test.dev",
  created_at: "2026-01-01T00:00:00Z",
  channels: [],
  email_enabled: true,
};

async function mountView() {
  const pinia = makePinia();
  useAuthStore().token = "tok";
  const router = makeRouter();
  const wrapper = mount(ProfileView, { global: { plugins: [pinia, router] } });
  await flushPromises();
  return wrapper;
}

beforeEach(() => vi.clearAllMocks());

describe("ProfileView (ADR-061)", () => {
  it("shows the email and the empty state, then lists channels", async () => {
    mocked.getProfile.mockResolvedValueOnce(emptyProfile).mockResolvedValueOnce({
      ...emptyProfile,
      channels: [{ id: "c1", kind: "telegram", target: "chat-42", created_at: "x" }],
    });
    const wrapper = await mountView();

    expect(wrapper.find('[data-testid="profile-email"]').text()).toBe("me@test.dev");
    expect(wrapper.find('[data-testid="no-channels"]').exists()).toBe(true);
  });

  it("adds a telegram channel with its bot token", async () => {
    mocked.getProfile.mockResolvedValue(emptyProfile);
    mocked.addChannel.mockResolvedValue({
      id: "c1",
      kind: "telegram",
      target: "chat-42",
      created_at: "x",
    });
    const wrapper = await mountView();

    await wrapper.find('[data-testid="channel-kind"]').setValue("telegram");
    await wrapper.find(".add-channel input").setValue("chat-42");
    // The token field only appears for telegram.
    await wrapper.find('input[type="password"]').setValue("bot-secret");
    await wrapper.find('[data-testid="add-channel-form"]').trigger("submit");
    await flushPromises();

    expect(mocked.addChannel).toHaveBeenCalledWith("telegram", "chat-42", "tok", "bot-secret");
  });

  it("offers and adds an email channel when SMTP is enabled (ADR-062)", async () => {
    mocked.getProfile.mockResolvedValue(emptyProfile);
    mocked.addChannel.mockResolvedValue({
      id: "c2",
      kind: "email",
      target: "me@example.com",
      created_at: "x",
    });
    const wrapper = await mountView();

    const options = wrapper.findAll('[data-testid="channel-kind"] option').map((o) => o.text());
    expect(options).toContain("Email");

    await wrapper.find('[data-testid="channel-kind"]').setValue("email");
    await wrapper.find(".add-channel input").setValue("me@example.com");
    await wrapper.find('[data-testid="add-channel-form"]').trigger("submit");
    await flushPromises();

    // No secret for email.
    expect(mocked.addChannel).toHaveBeenCalledWith("email", "me@example.com", "tok", undefined);
  });

  it("hides the email option when SMTP is disabled", async () => {
    mocked.getProfile.mockResolvedValue({ ...emptyProfile, email_enabled: false });
    const wrapper = await mountView();
    const options = wrapper.findAll('[data-testid="channel-kind"] option').map((o) => o.text());
    expect(options).not.toContain("Email");
  });

  it("surfaces a validation error from the API", async () => {
    mocked.getProfile.mockResolvedValue(emptyProfile);
    mocked.addChannel.mockRejectedValue(new ApiError(422, "slack target must be an https"));
    const wrapper = await mountView();

    await wrapper.find(".add-channel input").setValue("http://bad");
    await wrapper.find('[data-testid="add-channel-form"]').trigger("submit");
    await flushPromises();

    expect(wrapper.find('[data-testid="channel-error"]').text()).toContain("https");
  });

  it("deletes a channel", async () => {
    mocked.getProfile.mockResolvedValue({
      ...emptyProfile,
      channels: [{ id: "c9", kind: "slack", target: "https://hooks/x", created_at: "x" }],
    });
    mocked.deleteChannel.mockResolvedValue(undefined);
    const wrapper = await mountView();

    await wrapper.find('[data-testid="channel-c9"] .delete').trigger("click");
    await flushPromises();

    expect(mocked.deleteChannel).toHaveBeenCalledWith("c9", "tok");
  });
});
