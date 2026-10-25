<p align="center">
  <img src="docs/assets/wiesel.png" alt="Wiesel's launcher with five numbered quick actions (Fix spelling, Rewrite, Chat, Summarize, Explain) and a Text captured status" width="648">
</p>

<h1 align="center">Wiesel</h1>

<p align="center">
  <strong>Select text in any app, press a shortcut, and pick an AI action.</strong><br>
  A native macOS app for fixing, rewriting and asking about text.
</p>

<p align="center">
  <a href="https://github.com/zeekrey/wiesel/releases/latest"><strong>Download for macOS</strong></a> ·
  <a href="https://wiesel.run">wiesel.run</a>
</p>

---

## Why Wiesel?

When a sentence in an email, doc or Slack message needs work, you don't have to paste it into a chat window. Highlight it, press **⌘⇧Space**, then press **1** to fix the spelling.

- A single number key fixes the spelling of the selected text, rewrites it, summarizes it or explains it.
- Chat opens a conversation about your selection.
- The launcher shows 3, 5 or 8 quick actions. Each one has its own prompt, icon and model, and you can add your own.
- An action can paste its result over your selection.
- Models run on your Mac through [Ollama](https://ollama.com), or in the cloud with a Wiesel account.

## Install

1. Download the latest `.dmg` from [Releases](https://github.com/zeekrey/wiesel/releases/latest).
   - Apple Silicon (M1 and later): `macos-arm64`
   - Intel Mac: `macos-x86_64`
2. Open it and drag Wiesel into Applications.
3. Launch Wiesel.

## Setup

**1. Pick your shortcut.** The default is **⌘⇧Space**. Press a different key combination to record your own.

**2. Allow Accessibility.** Wiesel needs this permission to copy your selected text.
Open **System Settings → Privacy & Security → Accessibility** and switch on Wiesel.
Wiesel picks up the change without a restart.

**3. Choose a provider.** In **Settings**, pick a provider tab.

| | Wiesel (cloud) | Ollama (local) |
|---|---|---|
| Setup | Click **Login / Sign up** and finish in your browser | Install [Ollama](https://ollama.com) and pull a model, for example `ollama pull qwen3.5:9b` |
| Account | An account at [wiesel.run](https://wiesel.run) | None |
| Your text goes to | Wiesel and its model provider | Nowhere. It stays on your Mac |

**4. Assign your models.** Wiesel has three speed tiers, and you can pick any model for each one.

- Swift is the fastest. Use it for quick fixes and short replies.
- Sharp sits between the two. Actions use it by default.
- Sage is the slowest and most capable. Use it for harder problems.

## Usage

| Shortcut | Action |
|---|---|
| Select text, press **⌘⇧Space** | Open Wiesel with your selection loaded |
| **1** to **9** | Run the matching quick action |
| **⌘N** | Start a new chat |
| **⌘,** | Open Settings |
| **Esc** | Go back, or hide Wiesel |
| **⌘Q** | Quit |

Wiesel shows each result in its window. Review it there and click **Copy**. If you turn on **Paste result back** for an action, Wiesel pastes the result over your selection instead.

### Custom quick actions

Choose **File → New Quick Action…** or **Settings → Quick actions → Manage…**. Give the action a name, pick a [Lucide icon](https://lucide.dev/icons) by name or tag, and write its instructions. The instructions can use two placeholders.

- `{selection}` is the text you selected.
- `{clipboard}` is the current clipboard content.

```text
Translate the following into friendly, casual German:

{selection}
```

Each action uses either a tier (Swift, Sharp or Sage) or a specific model. To edit or remove any action, including the built-in ones, choose **Wiesel → Adjust Quick Actions…**.

## Privacy

- Wiesel sends text only when you run an action or click **Send**.
- With Ollama, no text leaves your Mac.
- Chats and results stay in memory. Wiesel writes none of them to disk.
- Wiesel restores your clipboard after it copies your selection.
- Wiesel stores login tokens in the macOS Keychain.

## FAQ

<details>
<summary><strong>Wiesel says it needs Accessibility, but I already allowed it.</strong></summary>

This usually happens after an update. Remove Wiesel from the Accessibility list with the **–** button, add it again with **+**, then relaunch Wiesel.
</details>

<details>
<summary><strong>Nothing happens when I press the shortcut.</strong></summary>

Another app may already use that key combination. Pick a different one in **Settings**.
</details>

<details>
<summary><strong>It doesn't work in a certain app.</strong></summary>

Wiesel reads your selection by copying it. It can't read password fields or apps that block copying. Paste the text into Chat instead.
</details>

<details>
<summary><strong>Ollama shows "not running".</strong></summary>

Open the Ollama app or run `ollama serve`, then click **Check** in Settings.
</details>

<details>
<summary><strong>Does it work on Windows or Linux?</strong></summary>

No. Wiesel runs only on macOS.
</details>
