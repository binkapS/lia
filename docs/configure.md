# Configure

`lia` reads `~/.config/lia/config.toml`. Create one with:

```sh
lia config --write ~/.config/lia/config.toml
```

Then set the model and provider:

```toml
provider = "openai"          # or "anthropic", "ollama", "ollama-cloud"
model = "gpt-4o-mini"
base_url = "https://api.openai.com/v1"

[approval]
policy = "ask"               # ask before running anything risky
```

API keys come from the environment or from the secrets file that
`lia setup` writes, never from the config file. The quickest way to set
everything up is the wizard:

```sh
lia setup
```

It asks for the provider's key and for any messaging bot tokens, and keeps
them in `~/.config/lia/secrets.toml` with owner-only permissions. An
exported variable still wins over the stored one, so existing setups keep
working:

```sh
export OPENAI_API_KEY=...     # for the openai provider
export ANTHROPIC_API_KEY=...  # for the anthropic provider
export OLLAMA_API_KEY=...     # for the ollama-cloud provider
```

The file can also be changed without the wizard:

```sh
lia setup --show                              # what is stored, masked
lia setup --set OPENAI_API_KEY=sk-...         # store one value
lia setup --unset TELEGRAM_BOT_TOKEN          # forget one
```

The file lives beside the config file when `--config` names one, and
`LIA_SECRETS_FILE` overrides the location.

The state directory — sessions, memory, managed skills — defaults to
`~/.local/share/lia` and can be moved with `--state-dir` or `LIA_STATE_DIR`.

The model may be left out, in which case the provider's own default is
used — the one that makes sense for it.

### Any OpenAI-compatible endpoint

Because most providers now speak the same API, one adapter covers a lot of
ground. Point `base_url` at whichever you use:

| Provider | `base_url` |
|---|---|
| OpenAI | `https://api.openai.com/v1` |
| MiniMax | `https://api.minimax.chat/v1` |
| Groq | `https://api.groq.com/openai/v1` |
| Together | `https://api.together.xyz/v1` |
| DeepSeek | `https://api.deepseek.com/v1` |
| OpenRouter | `https://openrouter.ai/api/v1` |
| LM Studio (local) | `http://localhost:1234/v1` |
| vLLM (local) | `http://localhost:8000/v1` |

For Anthropic, use `provider = "anthropic"` — the Messages API differs enough
from the OpenAI shape that it gets its own adapter. For Ollama, see below.

### Ollama

Ollama runs models on your own machine, and offers the same models in the
cloud. Both are one provider, because both speak the same API:

```toml
provider = "ollama"          # a server of your own
model = "qwen3.5:latest"
```

```toml
provider = "ollama-cloud"    # Ollama's cloud
model = "gpt-oss:120b"
```

A local server is found at `http://localhost:11434` and needs no key. The
cloud is at `https://ollama.com` and needs `OLLAMA_API_KEY` in the
environment. A server somewhere else is named with `base_url`; the paths
people are used to typing are understood too, so
`http://localhost:11434/v1` works as well.

The names of the models are the ones Ollama itself uses, and `lia models`
lists them:

```sh
$ lia models
gpt-oss:20b
qwen3.5:latest
```

A cloud model is named one way in the Ollama app and another in its API —
`gemma4:cloud` in the first, `gemma4:31b` in the second. A local server
that is signed in to Ollama takes the app's names, and the cloud takes the
API's. When the name that was asked for is not there, the other form of it
is tried, so both usually work.

Two settings are of the provider's own:

```toml
[ollama]
keep_alive = "10m"     # how long a model stays loaded after answering
think = true           # whether the model thinks before it answers
```

A model that thinks says so separately from its answer, and the two are
kept apart: the thinking is shown dimmed in the terminal, and the answer is
what is sent to a chat.

A local model is loaded into memory before it can answer, which takes a
while the first time. `keep_alive` keeps it there afterwards, so the
second question does not wait for it again.

### When a model cannot answer

One model is one point of failure: it goes down, runs out of credit, or
changes its name, and the assistant stops answering. Name the others to
try, and they are asked in turn:

```toml
[[fallback]]
provider = "anthropic"
model = "claude-sonnet-4-5"

[[fallback]]
provider = "ollama"
model = "llama3.2"
base_url = "http://localhost:11434"
```

The next model is asked only when the one before could not answer — a rate
limit, a network failure, a server in trouble, a model that is not there
any more. A request the provider refused as invalid is not tried again
elsewhere: it would be refused the same way. Keys still come from the
environment; `api_key_env` says which variable to read, so a second
endpoint can have a key of its own.

Before the next model is asked, the one that failed is asked once more,
after a pause. A hiccup is worth waiting out; a refusal is not:

```toml
[retry]
attempts = 3       # how many times a request is sent in all
backoff_ms = 500   # the first pause; the later ones are longer
```

Only the failures that might pass are tried again: a network error, a rate
limit, a server in trouble, a model that is not there. A request the
provider rejected is not repeated, and the pause grows with each try, so a
provider that is down for a while is not hammered.
