import { Schema } from "./mod.ts";
import { assertSnapshot } from "@std/testing/snapshot";

Deno.test("kitchenSink", async (t) => {
  const s = new Schema();

  const createdAt = s.scalar("created_at", "TIMESTAMPTZ").default("now()");
  const username = s.scalar("user_name", "TEXT");
  const groupId = s.scalar("group_id", "TEXT");

  const group = s.table("Group").with((t) => {
    t.column("group_id", groupId).primaryKey();
  });

  s.table("User").with((t) => {
    t.column(
      "user_id",
      s.scalar("user_id", "TEXT").inline(),
    ).primaryKey();
    t.column("group", groupId).fk(group.column("group_id"));
    t.column("user_name", username).optional();
    t.column("created_at", createdAt);
  });

  await assertSnapshot(t, s.toString());
});
