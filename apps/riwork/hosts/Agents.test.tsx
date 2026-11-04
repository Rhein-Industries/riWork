import { expect, open, test } from "nsh/test";
test("HOSTS-03 overview filters requests and opens the exact host session", async () => {
  const app=await open("./Agents.fixture.tsx",{platform:"macos",width:1000,height:650,host:{handlers:{"app.fixture.open":null,"app.fixture.notice":null}}});
  try {
    await app.waitFor("id:agent-host-a-chat-same");
    await app.press("id:agent-host-a-chat-same");
    const opened=app.host.posts().find(post => post.name === "app.fixture.open");
    expect(opened?.data).toEqual({host:"host-a",connection:"route-a",session:"same"});
    await app.press("id:agents-filter-attention");
    await app.waitFor("id:agent-host-a-chat-question");
    await app.press("id:agent-host-a-chat-question");
    expect(app.host.posts().filter(post => post.name === "app.fixture.open").length).toBe(2);
  } finally { app.dispose(); }
});
