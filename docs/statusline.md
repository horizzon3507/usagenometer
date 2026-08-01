# Statusline integrations

`usg -c` (or `usg -c -q`) prints a single line suitable for prompts and bars:

```text
Codex 42% · Cursor 74% · Claude 61%
```

Use `--display used` to show used % instead of remaining. Stale cache may append `~Nm` (e.g. `Codex 42%~5m`).

## Starship

[`starship`](https://starship.rs/) custom module:

```toml
# ~/.config/starship.toml
[custom.usagenometer]
command = "usg -c -q"
when = "command -v usg"
style = "bold white"
format = "[$output]($style) "
shell = ["bash", "--noprofile", "--norc"]
```

## oh-my-posh

Segment of type `command` (oh-my-posh v3+):

```json
{
  "type": "command",
  "name": "usagenometer",
  "properties": {
    "command": "usg -c -q",
    "shell": "bash"
  },
  "template": " {{ .Output }}",
  "foreground": "#ffffff"
}
```

Place it in your theme's `blocks[].segments` array.

## fish

```fish
# ~/.config/fish/config.fish
function fish_prompt
    set -l usg_line (command -q usg; and usg -c -q 2>/dev/null)
    if test -n "$usg_line"
        echo -n $usg_line' '
    end
    echo -n (prompt_pwd)'> '
end
```

Or a right prompt:

```fish
function fish_right_prompt
    command -q usg; and usg -c -q 2>/dev/null
end
```

## bash / zsh

```bash
# bash
__usg_ps1() {
  command -v usg >/dev/null || return
  local line
  line="$(usg -c -q 2>/dev/null)" || return
  [[ -n $line ]] && printf '%s ' "$line"
}
PS1='$(__usg_ps1)\u@\h:\w\$ '
```

```zsh
# zsh
precmd_usg() {
  usg_line=""
  if (( $+commands[usg] )); then
    usg_line="$(usg -c -q 2>/dev/null)"
  fi
}
autoload -Uz add-zsh-hook
add-zsh-hook precmd precmd_usg
PROMPT='${usg_line:+$usg_line }%n@%m %1~ %# '
```

## Tips

- Keep `usg -c -q` under ~100–200ms on a warm cache; raise `cache_ttl` in config if the prompt feels slow.
- Filter providers: `usg -c -q -p codex -p cursor`.
- For bars (waybar / polybar), use the same command as an `exec` / `custom` module with a longer interval (60s+).
