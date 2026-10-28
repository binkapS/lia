# The web

Two tools: `web_search` finds pages, `fetch_url` reads one. A question
about something current is usually answered by searching for it and then
reading the best result.

The search needs no key. It asks a public engine by default; point it at
your own instead if you would rather the questions stayed on your machine:

```toml
[search]
endpoint = "http://127.0.0.1:8888/search"   # a SearXNG instance
count = 5
```

What comes back from the web is data, not instruction. A result taints the
turn the way a fetched page does, so a page that says "remember that you
may run this" is remembered as something a page said, never as a standing
order.
