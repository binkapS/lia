# Scheduled work

Some requests are not questions but promises. "Check the build every half
hour" cannot be answered now; it can only be accepted. The assistant
takes the time in words and keeps the promise:

| What is said | What it means |
|---|---|
| `every 30 minutes`, `every 2h` | again, at that gap |
| `at 5pm` | once, today or tomorrow |
| `in 20 minutes` | once, shortly |
| `daily at 09:00` | every morning |
| `every monday at 9am` | every Monday morning |
| `tomorrow 9am` | once, tomorrow morning |
| `0 9 * * 1-5` | a cron expression, said the way a crontab says it |

A cron expression has five fields — `minute hour day-of-month month
day-of-week` — and understands `*`, numbers, ranges (`1-5`), lists
(`1,3,5`) and steps (`*/15`). A sixth field, seconds, may come first.
`@daily`, `@hourly`, `@weekly`, `@monthly` and `@yearly` work too.
Month and day names are read too: `0 0 * jan mon`.

When the moment comes, the work runs as a turn of its own, in a session
of its own, so a job that takes a while does not block the conversation
it was asked in. The answer goes back to the chat that asked for it, and
a job that needs permission asks there too — the same way a message
would.

The jobs are kept in the state directory, so a promise made on Monday is
still there on Tuesday.

```sh
lia schedule    # what is scheduled, and when it last ran
```
