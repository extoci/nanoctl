import { defineSchema, defineTable } from "convex/server";
import { v } from "convex/values";

export default defineSchema({
  devices: defineTable({
    ownerId: v.string(),
    name: v.string(),
    platform: v.union(v.literal("windows"), v.literal("macos"), v.literal("linux")),
    architecture: v.union(v.literal("x64"), v.literal("arm64")),
    agentVersion: v.string(),
    status: v.union(v.literal("online"), v.literal("offline"), v.literal("disabled")),
    tokenHash: v.string(),
    capabilitiesJson: v.string(),
    lastSeenAt: v.number(),
    createdAt: v.number(),
    disabledAt: v.optional(v.number()),
  })
    .index("by_owner", ["ownerId"])
    .index("by_token_hash", ["tokenHash"]),

  pairingCodes: defineTable({
    ownerId: v.string(),
    codeHash: v.string(),
    expiresAt: v.number(),
    createdAt: v.number(),
    consumedAt: v.optional(v.number()),
  })
    .index("by_code_hash", ["codeHash"])
    .index("by_owner", ["ownerId"]),

  sessions: defineTable({
    ownerId: v.string(),
    deviceId: v.id("devices"),
    state: v.union(
      v.literal("requested"),
      v.literal("ringing"),
      v.literal("negotiating"),
      v.literal("connected"),
      v.literal("ended"),
      v.literal("failed"),
    ),
    expiresAt: v.number(),
    createdAt: v.number(),
    updatedAt: v.number(),
    endedAt: v.optional(v.number()),
    endReason: v.optional(v.string()),
  })
    .index("by_owner", ["ownerId"])
    .index("by_device_state", ["deviceId", "state"]),

  signals: defineTable({
    sessionId: v.id("sessions"),
    sender: v.union(v.literal("controller"), v.literal("host")),
    kind: v.union(
      v.literal("offer"),
      v.literal("answer"),
      v.literal("ice-candidate"),
      v.literal("ice-complete"),
      v.literal("end"),
    ),
    sequence: v.number(),
    envelope: v.string(),
    expiresAt: v.number(),
    createdAt: v.number(),
  })
    .index("by_session_sender_sequence", ["sessionId", "sender", "sequence"])
    .index("by_session_sender_kind_sequence", ["sessionId", "sender", "kind", "sequence"])
    .index("by_expiry", ["expiresAt"]),

  auditEvents: defineTable({
    ownerId: v.string(),
    deviceId: v.optional(v.id("devices")),
    sessionId: v.optional(v.id("sessions")),
    action: v.string(),
    detail: v.optional(v.string()),
    createdAt: v.number(),
  }).index("by_owner_time", ["ownerId", "createdAt"]),
});
