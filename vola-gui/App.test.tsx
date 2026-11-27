import { expect, open, test } from "nsh/test";

test("shows the connection screen before a core is reachable", async () => {
  const app = await open("./App.tsx");
  const texts = app.texts();
  expect(texts).toContain("Vola");
  expect(texts).toContain("Connect");
  expect(texts).toContain("Start core");
});
