# Voice

lia can listen and speak. Speaking has a neural voice built in: **Phonon**
runs in this process, with no Python and no PyTorch, and sounds like a
person. Hearing uses programs on the machine: the sound is recorded by one
and turned into words by another. Each can be named in the configuration,
or replaced by an HTTP endpoint.

What is said is composed, not read off the transcript: the terminal uses
the same turns as the browser page — a courtesy to open ("Right away.",
"Of course, sir."), lists read "first… then… and finally…", code and wide
tables pointed at instead of recited, and the units a screen can skim
given as words the ear can carry.

| job | what is looked for |
|---|---|
| record | `pw-record`, `arecord`, `ffmpeg`, `sox` |
| transcribe | `whisper-cli` (whisper.cpp), `whisper` |
| speak | `phonon` (built in), `piper`, `spd-say`, `espeak-ng` |
| play | `gst-play-1.0`, `pw-play`, `ffplay`, `mpv`, `mplayer`, `cvlc` |

`lia doctor` says which of them were found.

To hear and be heard on this machine, whisper.cpp and a model are needed.
The release assets are Windows binaries, so on Linux it is built from
source:

```sh
git clone --depth 1 --branch v1.8.0 https://github.com/ggml-org/whisper.cpp
cmake -S whisper.cpp -B whisper.cpp/build \
  -DCMAKE_BUILD_TYPE=Release -DWHISPER_BUILD_TESTS=OFF \
  -DWHISPER_BUILD_EXAMPLES=ON \
  -DCMAKE_BUILD_WITH_INSTALL_RPATH=ON -DCMAKE_INSTALL_RPATH='$ORIGIN'
cmake --build whisper.cpp/build --target whisper-cli -j

# The binary and its libraries go together, so that it finds them.
mkdir -p ~/.local/share/lia/whisper
cp whisper.cpp/build/bin/whisper-cli ~/.local/share/lia/whisper/
cp whisper.cpp/build/src/libwhisper.so* whisper.cpp/build/ggml/src/libggml*.so* \
  ~/.local/share/lia/whisper/
ln -sf ~/.local/share/lia/whisper/whisper-cli ~/.local/bin/whisper-cli

# a model, about 150 MB
curl -L -o ~/.local/share/lia/models/ggml-base.en.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin
```

The model is looked for in `~/.local/share/lia/models` and a few other
places; `voice.transcribe_model` names one explicitly.

A transcriber named in `voice.transcribe_command` is given the wav by the
name `{file}`; one without it reads the wav on its standard input. The
difference matters for whisper.cpp, which says nothing at all when the
sound arrives on its standard input.

### The voice

The voice is a neural one, so that it sounds like a person and not like a
machine. Phonon is built in, so nothing has to be installed: when a player
is present and nothing else has been configured or found, lia speaks with
it. The first request downloads the default model, Phonon 31m (English),
from Hugging Face — about 65 MB, cached and reused afterwards. Nothing
goes over the network after that.

```toml
[voice]
# Optional: Phonon is chosen by itself when a player is found.
speak_provider = "phonon"
# Optional: the default is Phonon 31m, pinned and downloaded on first use.
speak_model = "hf://gradium/phonon-31m-en"
# Optional: a voice from the Phonon catalog, or a voice .safetensors file.
voice = "Juniper"
```

The model is a small checkpoint rather than a service: synthesis runs on
the CPU in this process, and the audio starts playing before the whole
answer has been made. Build with `--release` for speech: the debug build
is too slow to keep up.

Five languages are spoken by the 40m checkpoints — English, French,
German, Spanish and Portuguese — and the multilingual one speaks all of
them: `hf://gradium/phonon-40m-multilingual`. Each one-language checkpoint
is named for its language (`hf://gradium/phonon-40m-fr`, and `-de`,
`-es`, `-pt`). The 40m models are gated: accept their terms on Hugging
Face first and set `HF_TOKEN` or run `hf auth login`. Phonon 31m is under
the Apache 2.0 license; the 40m models are under CC BY-NC 4.0.

A voice the checkpoint does not have is looked up in the
[Phonon voice catalog](https://huggingface.co/gradium/phonon-voices) and
only that voice is downloaded. `voice` in the `[voice]` table can also
name a voice file downloaded from Gradium Studio. Pick a voice in the
language of the text; the name is matched without regard to case.

`voice.speak_model` also accepts a local checkpoint directory, which needs
no Hub access once its files are there. When the model cannot be loaded —
offline on the first run, say — the reason is reported rather than a
silence.

#### piper instead

[piper](https://github.com/rhasspy/piper) is another neural voice, and can
be used instead of Phonon. It is a program rather than a library, so it
needs its own Python and libraries:

```sh
# piper, with its own python and its own libraries
mkdir -p ~/.local/share/piper
curl -L https://github.com/rhasspy/piper/releases/download/2023.11.14-2/piper_linux_x86_64.tar.gz \
  | tar xz -C ~/.local/share/piper
ln -sf ~/.local/share/piper/piper/piper ~/.local/bin/piper

# a voice, about 63 MB. The two files go together: the second one tells
# piper how the first one is to be read.
base=https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/hfc_female/medium
curl -L -o ~/.local/share/lia/models/en_US-hfc_female-medium.onnx \
  $base/en_US-hfc_female-medium.onnx
curl -L -o ~/.local/share/lia/models/en_US-hfc_female-medium.onnx.json \
  $base/en_US-hfc_female-medium.onnx.json
```

The voices are looked for in `~/.local/share/lia/models` and a few other
places, and the one that sounds most like a person is used;
`voice.speak_model` names one explicitly, and `voice.speak_options` gives
the engine arguments of its own — `--length_scale 1.1`, for a voice that
speaks a little more slowly.

What is said is said as prose: the marks of writing — the fence around a
piece of code, the stars around a word, the address of a link — are taken
away before the words are read out, because a voice that reads them says
things that nobody wrote.

The sound is played by a player of its own, which is given the wav by the
name `{file}`; `voice.play_command` names a different one.

In the terminal:

```sh
lia listen               # record, and print the words
lia speak "hello there"  # say a line
```

In the TUI, `F2` opens the microphone. The level of what it hears is shown
at the bottom, and the words are sent as your message when the recording
ends — which is when you stop talking, not at a fixed time.

| Command | What it does |
|---|---|
| `/listen` | record, and send the words |
| `/dictate` | record, and put the words in the box so you can edit them |
| `/speak <words>` | say a line out loud |
| `/voice` | what was found for hearing and speaking |
| `/voice on` / `/voice off` | read the answers aloud (the default), or keep them silent |
| `/handsfree on` / `/handsfree off` | listen, answer aloud, listen again |
| `/live on` / `/live off` | talk over the answer, with the microphone always open |
| `/live none` / `/live camera` / `/live screen` | what, besides the voice, the model is shown |

The answers are spoken by default, whenever the machine has a voice; the
microphone is the part that waits to be asked. `/voice off` keeps the
transcript silent for the rest of the visit.

Hands-free shuts the microphone while the answer is being said, so lia does
not answer itself. It also listens for her name: the room is full of talk,
and only the words that say "lia" are for her. The rest is passed over,
and the microphone opens again. `voice.wake_word` names her differently.

### Live mode

Live mode is the conversation the Gemini app calls Live: the microphone
stays open, nothing has to be named first, and the answer is spoken while
you are still there. Talking over her stops the answer instead of waiting
for it. What you said becomes the next question; if the agent is still
working on the previous turn, the words wait for it to finish.

```sh
lia live                 # voice only
lia live --share screen  # a screen frame goes with each question
```

In the terminal, `/live on` starts it and `/live off` stops it. `/live
camera` and `/live screen` turn it on with one frame of the camera or the
screen taken at the moment you speak — not a video stream, but what the
model is shown along with the words.

Because the microphone is open while she speaks, some of what it hears is
her own voice coming back through the room. `live.echo_similarity` decides
how much of a transcript has to match what she has just said for it to be
passed over as an echo; zero turns the guard off. A person who talks over
her brings words of their own, and those words fall below the threshold.

```toml
[live]
share = "screen"
screen_command = ["grim", "{file}"]
camera_command = ["ffmpeg", "-f", "v4l2", "-i", "/dev/video0",
                  "-frames:v", "1", "-y", "{file}"]
echo_similarity = 0.6
```

The capture command is given the path in `{file}`, or has the path put at
the end when the placeholder is absent. The picture must be a PNG or JPEG.

The `listen` tool opens the microphone too, so it asks for permission
first; the `speak` tool only makes a sound, so it does not.

An endpoint can be used instead of a program:

```toml
[voice]
transcribe_url = "http://127.0.0.1:8080/v1/audio/transcriptions"
transcribe_api_key_env = "LIA_TRANSCRIBE_API_KEY"
speak_url = "http://127.0.0.1:8080/v1/audio/speech"
speak_api_key_env = "LIA_SPEAK_API_KEY"
```

The keys are read from the environment, so they never land in a file.

### A voice of J.A.R.V.I.S's kind

The [J.A.R.V.I.S project](https://github.com/codewithbro95/J.A.R.V.I.S) speaks
through ElevenLabs: a model of `eleven_turbo_v2`, keyed by an
`ELEVENLABS_API_KEY`, with one particular voice asked by its own id. lia
speaks to the same service the same way, without a library of theirs:

```toml
[voice]
speak_provider = "elevenlabs"
speak_url = "https://api.elevenlabs.io/v1"
speak_api_key_env = "ELEVENLABS_API_KEY"
# voice = "a voice of your own, by its id"
```

```sh
export ELEVENLABS_API_KEY=...
```

Most of it is spelled for you. Naming the provider is not strictly needed
— an address with elevenlabs in it is recognised — and each default is of
the kind: the model becomes `eleven_turbo_v2`, the key is read from
`ELEVENLABS_API_KEY` when no other variable is named, and, when no voice
is named, the voice J.A.R.V.I.S itself speaks with is used. The base of
the API alone may be named (`https://api.elevenlabs.io`,
`https://api.elevenlabs.io/v1`, or the voice's own endpoint whole), and
the address that says the voice is worked out from it.

The same goes for the shape of the request, which is why a whole backend
is more than a different address: there the voice rides in the address,
the model is called `model_id` in the body, and the key is a header of
its own rather than a bearer. Every other part of the voice — the words
composed for speaking, the queue, the interruption, the hands-free mode —
is unchanged; only the way the sound is asked for is different.
`lia doctor` still says which voice was found.
