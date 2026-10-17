// Cloudflare Worker collector: cron fetches ActiveSG's public crowd snapshot
// from CF's own network and pushes it to the Railway ingest endpoint.
// Verdict on whether CF egress passes ActiveSG's Cloudflare zone is logged
// per run.

async function poll(env) {
  const r = await fetch(env.SOURCE_URL, {
    headers: {
      "user-agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
      "accept": "*/*"
    }
  });
  const body = await r.text();
  if (!r.ok) {
    return JSON.stringify({ ok: false, status: r.status, head: body.slice(0, 100) });
  }
  const p = await fetch(env.INGEST_URL, {
    method: "POST",
    headers: {
      "authorization": "Bearer " + env.INGEST_TOKEN,
      "content-type": "application/json"
    },
    body
  });
  return JSON.stringify({ ok: p.ok, fetch_status: r.status, ingest_status: p.status, ingest: await p.text() });
}

export default {
  async scheduled(event, env, ctx) {
    ctx.waitUntil(poll(env));
  },
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === "/run") {
      // authenticated manual trigger so we can test on demand
      if (request.headers.get("authorization") !== "Bearer " + env.INGEST_TOKEN) {
        return new Response("bad token", { status: 401 });
      }
      return new Response(await poll(env), { headers: { "content-type": "application/json" } });
    }
    return new Response("ActiveSG collector worker - cron */10. POST-free; /run needs auth.", { status: 200 });
  }
};
