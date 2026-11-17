#!/usr/bin/env python3
"""Exercise real terminal workflows and shell Tab completion in disposable projects."""
import codecs
import re
import unicodedata
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import shlex
import shutil
import signal
import struct
import subprocess
import tempfile
import termios
import time

BINARY = Path(os.environ.get("DWS_BIN", "target/debug/dws")).resolve()


class Screen:
    """Small VT screen reader for the cursor/erase sequences emitted by Ratatui."""
    def __init__(self):
        self.width, self.height = 80, 24
        self.lines = [[" "] * self.width for _ in range(self.height)]
        self.row = self.col = 0
        self.pending = ""
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")

    def resize(self, width, height):
        self.lines = [(self.lines[r] if r < len(self.lines) else [])[:width] + [" "] * max(0, width - (len(self.lines[r]) if r < len(self.lines) else 0)) for r in range(height)]
        self.width, self.height = width, height
        self.row = min(self.row, height - 1)
        self.col = min(self.col, width - 1)

    def feed(self, data):
        self.pending += self.decoder.decode(data)
        while self.pending:
            if self.pending.startswith("\x1b"):
                if len(self.pending) < 2:
                    return
                if self.pending[1] == "[":
                    match = re.match(r"\x1b\[([?0-9;:]*)([ -/]*)([@-~])", self.pending)
                    if not match:
                        return
                    values, _, command = match.groups()
                    args = [int(v or 0) for v in values.lstrip("?").split(";")]
                    number = args[0] or 1
                    if command in "Hf":
                        self.row = max(0, min(self.height - 1, (args[0] or 1) - 1))
                        self.col = max(0, min(self.width - 1, (args[1] if len(args) > 1 and args[1] else 1) - 1))
                    elif command == "G": self.col = min(self.width - 1, number - 1)
                    elif command == "A": self.row = max(0, self.row - number)
                    elif command == "B": self.row = min(self.height - 1, self.row + number)
                    elif command == "C": self.col = min(self.width - 1, self.col + number)
                    elif command == "D": self.col = max(0, self.col - number)
                    elif command == "J" and args[0] in (2, 3): self.lines = [[" "] * self.width for _ in range(self.height)]
                    elif command == "K":
                        start, end = (0, self.width) if args[0] == 2 else ((0, self.col + 1) if args[0] == 1 else (self.col, self.width))
                        self.lines[self.row][start:end] = [" "] * (end - start)
                    elif command == "h" and values == "?1049":
                        self.lines = [[" "] * self.width for _ in range(self.height)]
                        self.row = self.col = 0
                    self.pending = self.pending[match.end():]
                elif self.pending[1] == "]":
                    end = self.pending.find("\x07")
                    if end < 0: return
                    self.pending = self.pending[end + 1:]
                else:
                    self.pending = self.pending[2:]
                continue
            char, self.pending = self.pending[0], self.pending[1:]
            if char == "\r": self.col = 0
            elif char == "\n": self.row += 1
            elif char == "\b": self.col = max(0, self.col - 1)
            elif char == "\t": self.col = min(self.width - 1, (self.col // 8 + 1) * 8)
            elif char >= " ":
                if self.col >= self.width: self.col = 0; self.row += 1
                if self.row >= self.height: self.lines.pop(0); self.lines.append([" "] * self.width); self.row = self.height - 1
                self.lines[self.row][self.col] = char
                self.col += 2 if unicodedata.east_asian_width(char) in "WF" else 1
            if self.row >= self.height:
                self.lines.pop(0); self.lines.append([" "] * self.width); self.row = self.height - 1

    def text(self):
        return "\n".join("".join(row) for row in self.lines)


class Terminal:
    def __init__(self, args, cwd, env, width=80, height=24):
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.chdir(cwd)
            os.execvpe(args[0], args, env)
        self.screen = Screen()
        self.output = b""
        self.closed = False
        self.resize(width, height)

    def resize(self, width, height):
        self.screen.resize(width, height)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))
        os.kill(self.pid, signal.SIGWINCH)

    def read(self, duration=0.1):
        end = time.monotonic() + duration
        while time.monotonic() < end:
            ready, _, _ = select.select([self.fd], [], [], min(0.05, max(0, end - time.monotonic())))
            if not ready:
                continue
            try:
                data = os.read(self.fd, 65536)
            except OSError as error:
                if error.errno == errno.EIO:
                    break
                raise
            if not data:
                break
            self.output += data
            self.screen.feed(data)
        return self.output

    def send(self, text):
        os.write(self.fd, text.encode() if isinstance(text, str) else text)

    def click_text(self, text, count=1, checkbox=False, word=False, in_list=False):
        """Click visible text; `checkbox` targets the row's selection marker and
        `word` skips matches inside longer words (such as "esc" in "Description");
        `in_list` only matches list rows, which carry a selection marker."""
        self.read()
        markers = "○●◆✕"
        for row, cells in enumerate(self.screen.lines):
            line = "".join(cells)
            match = re.search(rf"(?<!\w){re.escape(text)}(?!\w)" if word else re.escape(text), line)
            # A list item's marker sits two columns before its name.
            marker = match.start() - 2 if match and match.start() >= 2 and line[match.start() - 2] in markers else None
            if match and (not (checkbox or in_list) or marker is not None):
                col = marker if checkbox else match.start()
                for _ in range(count):
                    self.send(f"\x1b[<0;{col+1};{row+1}M\x1b[<0;{col+1};{row+1}m")
                self.read()
                return
        raise AssertionError(f"no clickable {text!r}: {self.screen.text()!r}")

    def expect(self, text, since=0, timeout=10, screen=True):
        expected = text.encode() if isinstance(text, str) else text
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            self.read()
            if expected in self.output[since:] or (screen and text in self.screen.text()):
                return
        raise AssertionError(f"terminal missing {text!r}: {self.screen.text()!r}\nRAW: {self.output[since:][-2000:]!r}")

    def finish(self, expected=0):
        end = time.monotonic() + 10
        while time.monotonic() < end:
            self.read()
            pid, status = os.waitpid(self.pid, os.WNOHANG)
            if pid:
                self.closed = True
                os.close(self.fd)
                code = os.waitstatus_to_exitcode(status)
                assert code == expected, (code, self.output[-2000:])
                return
        raise AssertionError("terminal child did not exit")

    def close(self):
        if not self.closed:
            try:
                os.kill(self.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(self.pid, 0)
            os.close(self.fd)
            self.closed = True


def run(root, env, *args):
    return subprocess.run([str(BINARY), *args], cwd=root, env=env, text=True, capture_output=True, check=True).stdout


def create(root, env, name, repo="api"):
    file = root / "manifest.json"
    file.write_text(json.dumps({"schema_version": 1, "name": name, "repos": [{"path": repo, "kind": "link"}]}))
    return run(root, env, "create", "--file", str(file)).strip()


def capture(term, name):
    destination = os.environ.get("DWS_CAPTURE_DIR")
    if destination:
        path = Path(destination)
        path.mkdir(parents=True, exist_ok=True)
        (path / f"{name}.json").write_text(json.dumps({"width":term.screen.width, "height":term.screen.height, "lines":term.screen.lines}))


def open_with_tests(root, env):
    """Exercise the real TUI and CLI against a disposable T3 subprocess."""
    env = dict(env)
    home = root / "fake-home"
    binary = home / ".t3/bin/t3"
    binary.parent.mkdir(parents=True)
    record = root / "t3-calls.jsonl"
    fail = root / "t3-fail"
    binary.write_text("#!/usr/bin/env python3\nimport json,sys\nfrom pathlib import Path\n"
                      f"record=Path({str(record)!r}); fail=Path({str(fail)!r})\n"
                      "with record.open('a') as f: f.write(json.dumps(sys.argv[1:])+'\\n')\n"
                      "if fail.exists(): print('T3 test failure: desktop unavailable',file=sys.stderr); sys.exit(7)\n"
                      )
    binary.chmod(0o755)
    env["HOME"] = str(home)
    term = Terminal([str(BINARY)], root, env)
    try:
        term.expect("Open a session")
        term.send(b"\x0f")
        term.expect("Choose where to open")
        for width, height in [(40,12), (60,16), (80,24), (120,40)]:
            term.resize(width, height)
            term.read(0.15)
            visible = term.screen.text()
            assert "T3 Code" in visible and "Finder" in visible and "Cancel" in visible and "Open" in visible, visible
            capture(term, f"open-with-{width}")
        term.send("2\r")
        term.expect("in T3 Code")
        term.expect(" app ")
        term.send(b"\r")  # Enter cancels the first-use confirmation.
        term.expect("Choose where to open")
        assert not record.exists()
        term.send(b" \r")  # remember, then confirm
        term.expect("in T3 Code")
        term.send("y")
        term.expect("Opened api-review")
        calls = [json.loads(line) for line in record.read_text().splitlines()]
        assert calls == [["app",str(root / ".dynws/workspaces/api-review")]], calls
        config = (root / ".dynws/config.toml").read_text()
        assert 'opener = "t3"' in config and "t3_confirmed = true" in config
        term.send(b"\x03")
        term.finish()
    finally:
        term.close()
    # Repeat opening runs the same single command, without preregistration.
    run(root, env, "open", "api-review")
    calls = [json.loads(line) for line in record.read_text().splitlines()]
    assert len(calls) == 2 and calls[-1][0] == "app", calls
    fail.touch()
    term = Terminal([str(BINARY)], root, env)
    try:
        term.expect("Open a session")
        term.send(b"\x14")  # direct T3 command
        term.expect("Could not open session")
        term.send(b"\x1bOQ")  # F2: full stderr
        term.expect("desktop unavailable")
        capture(term, "open-with-error")
        term.send(b"\x03")
        term.finish()
    finally:
        term.close()
    assert len(record.read_text().splitlines()) == 3
    binary.unlink()
    term = Terminal([str(BINARY)], root, env)
    try:
        term.expect("Open a session")
        term.send(b"\x14")
        term.expect("Could not open session")
        term.send(b"\x1bOQ")
        term.expect("CLI is missing")
        term.send(b"\x03")
        term.finish()
    finally:
        term.close()
    run(root, env, "config", "clear", "opener")
    manager = root / "fake-file-manager"
    revealed = root / "revealed-path"
    manager.write_text(f"#!/bin/sh\nprintf '%s' \"$1\" > {shlex.quote(str(revealed))}\n")
    manager.chmod(0o755)
    run(root, env, "config", "set", "file-manager", str(manager))
    term = Terminal([str(BINARY)], root, env)
    try:
        term.expect("Open a session")
        term.send(b"\x0f")
        term.expect("Choose where to open")
        term.click_text("Finder")
        term.click_text("Use this choice")
        term.send(b"\r")
        term.expect("Opened api-review")
        assert revealed.read_text() == str(root / ".dynws/workspaces/api-review")
        assert 'opener = "finder"' in (root / ".dynws/config.toml").read_text()
        term.send(b"\x03")
        term.finish()
    finally:
        term.close()
    run(root, env, "config", "set", "opener", "t3")
    # --editor overrides even a saved T3 default with a missing T3 binary.
    run(root, env, "open", "api-review", "--editor", str(manager))
    run(root, env, "config", "clear", "opener")
    run(root, env, "config", "clear", "file-manager")


def terminal_tests(root, env):
    editor = root / "editor with spaces"
    record = root / "editor.json"
    editor.write_text("#!/usr/bin/env python3\nimport json,sys,termios,time\nfrom pathlib import Path\n"
                      f"target=Path({str(record)!r})\n"
                      "attrs=termios.tcgetattr(0)\n"
                      "print('EDITOR_STARTED', flush=True)\ntime.sleep(0.1)\n"
                      "target.write_text(json.dumps({'args':sys.argv[1:],'canonical':bool(attrs[3]&termios.ICANON),'echo':bool(attrs[3]&termios.ECHO)}))\n")
    editor.chmod(0o755)
    run(root, env, "config", "set", "editor", f'"{editor}" "two words"')
    term = Terminal([str(BINARY)], root, env)
    try:
        term.expect("Open a session")
        term.send(b"\x0e")  # Ctrl+N enters shared create flow.
        term.expect("New session")
        term.send(b"\x1b")
        term.expect("New session cancelled")
        term.send(b"\x01")  # Ctrl+A enters shared Add flow from home.
        term.expect("Add repositories")
        term.send("worker")
        term.send(b"\x13\r")  # select link; commit addition
        term.expect("Repositories")
        term.send(b"\x1b")
        term.read(0.2)
        term.send(b"\x13")  # Bulk selection must survive filtering and every layout transition.
        term.send("api")
        term.read(0.2)
        for width, height in [(120,40), (100,20), (99,20), (100,19), (80,24), (60,16), (40,12), (39,12), (30,8), (120,12), (80,24)]:
            term.resize(width, height)
            term.read(0.2)
            visible = term.screen.text()
            if width < 40 or height < 12:
                assert "Resize to 40" in visible, (width, height, visible)
            else:
                assert "api-review" in visible and "1 selected" in visible, (width, height, visible)
                launcher = width >= 60 and height >= 20
                assert ("Open a session" in visible) == launcher, (width, height, visible)
                sidebar = re.search(r"dynws \d", visible) is not None
                assert sidebar == (width >= 100 and height >= 20 and not launcher), (width, height, visible)
        term.send(b"\x1b")  # Clear filter; preserve highlighted session and bulk selection.
        term.read(0.1)
        term.send(b"\x0f")
        term.expect("Choose where to open")
        term.send(b"\r")  # Chosen terminal editor opens after terminal teardown.
        term.expect("EDITOR_STARTED")
        term.finish()
        data = json.loads(record.read_text())
        assert data["canonical"] and data["echo"], data
        assert data["args"][0] == "two words"
        assert term.output.rfind(b"\x1b[?1049l") < term.output.index(b"EDITOR_STARTED")
        assert (root / ".dynws/workspaces/api-review/worker").is_symlink()
    finally:
        term.close()
    term = Terminal([str(BINARY), "new", "--no-open"], root, env)
    try:
        term.expect("New session")
        term.send("extra")
        term.send(b"\x13\r")
        term.expect("Letters, numbers and dashes")
        term.send("ui-created\r")
        term.finish()
        assert (root / ".dynws/sessions/ui-created.toml").is_file()
    finally:
        term.close()
    term = Terminal([str(BINARY)], root, env)
    try:
        term.expect("Open a session")
        term.send("ui-created")
        term.send(b"\x05")  # Ctrl+E rename
        term.expect("Rename ui-created")
        term.send(b"\x7f" * len("ui-created") + b"ui-renamed\r")
        term.expect("renamed ui-created")
        term.send("ui-renamed")
        term.send(b"\x04")
        term.expect("Delete ui-renamed?")
        term.send(b"\r")  # Enter cancels by default.
        term.expect("remove cancelled")
        assert (root / ".dynws/sessions/ui-renamed.toml").is_file()
        term.send(b"\x04")
        term.read(0.1)
        term.send("y")
        term.expect("removed 1 session")
        term.send(b"\x03")
        term.finish()
        assert not (root / ".dynws/sessions/ui-renamed.toml").exists()
    finally:
        term.close()


def mouse_tests(root, env):
    term = Terminal([str(BINARY)], root, env, width=120, height=40)
    try:
        term.expect("Open a session")
        term.send(b"\x10")
        term.expect("Commands")
        term.send("repositories")
        term.send(b"\x1b")
        term.read()
        # Palette search text never leaks into the session filter.
        assert "Search by name, repository or branch" in term.screen.text()
        term.click_text("api-review", checkbox=True)
        term.expect("1 selected")
        term.click_text("ctrl+p")
        term.expect("Commands")
        term.send("delete")
        term.click_text("Delete sessions")
        term.expect("Delete api-review?")
        term.read(0.4)  # A distinct click, outside the action coalescing interval.
        term.click_text("Cancel", word=True)
        term.expect("remove cancelled")
        assert (root / ".dynws/sessions/api-review.toml").is_file()
        term.read(0.4)
        term.click_text("ctrl+p")
        term.send("show repositories")
        term.click_text("Show repositories")
        term.expect("2 repositories")
        term.click_text("worker", in_list=True)
        term.read(0.4)
        term.click_text("f2", word=True)
        term.expect("Details")
        term.read(0.4)
        term.click_text("esc", word=True)
        term.send(b"\x1b")
        term.read(0.2)
        term.click_text("api-review", count=2, in_list=True)
        term.expect("EDITOR_STARTED")
        term.finish()
        assert term.output.rfind(b"\x1b[?1006l") < term.output.index(b"EDITOR_STARTED")
        assert b"\x1b[?1006h" in term.output and b"\x1b[?1006l" in term.output
        data = json.loads((root / "editor.json").read_text())
        assert data["canonical"] and data["echo"], data
    finally:
        term.close()
    term = Terminal([str(BINARY), "new", "--no-open"], root, env, width=40, height=12)
    try:
        term.expect("New session")
        term.click_text("extra")
        term.send(b"\x1b[12~")  # F2 updates when the background tree arrives.
        for _ in range(30):
            term.send(b"\x1b[F")  # End: the compact details view is scrollable.
            term.read()
            if "folder-preview.txt" in term.screen.text():
                break
        term.expect("folder-preview.txt")
        term.click_text("esc", word=True)
        term.click_text("extra", checkbox=True)
        term.click_text("Next", word=True)
        term.expect("Letters, numbers and dashes")
        term.send("mouse-created")
        term.read(0.4)
        term.click_text("Create session")
        term.finish()
        assert (root / ".dynws/sessions/mouse-created.toml").is_file()
    finally:
        term.close()


def worktree_workflow(root, env):
    origin = root / "origin.git"
    repo = root / "git-api"
    repo.mkdir()
    def git(cwd, *args):
        subprocess.run(["git", "-C", str(cwd), *args], env=env, capture_output=True, check=True)
    git(root, "init", "--bare", str(origin))
    git(repo, "init", "-b", "main")
    git(repo, "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "--allow-empty", "-m", "initial")
    git(repo, "remote", "add", "origin", str(origin))
    git(repo, "push", "-u", "origin", "main")
    # Delay only fetch to exercise a responsive source list while Git is busy.
    wrapper = root / "git-wrapper"
    wrapper.mkdir()
    real_git = shutil.which("git")
    assert real_git
    (wrapper / "git").write_text("#!/usr/bin/env python3\nimport os,sys,time\n"
                                 "if 'fetch' in sys.argv[1:]: time.sleep(1)\n"
                                 f"os.execv({real_git!r}, [{real_git!r}, *sys.argv[1:]])\n")
    (wrapper / "git").chmod(0o755)
    fetch_env = dict(env, PATH=f"{wrapper}:{env['PATH']}")
    term = Terminal([str(BINARY), "new", "--no-open"], root, fetch_env)
    try:
        term.expect("New session")
        term.send("git-api")
        term.send(b"\x17\r")  # Ctrl+W, then fetch/pick branches.
        term.expect("Fetching origin")
        term.send("-missing")  # Filtering continues without changing the pending selection.
        term.expect("No repositories match")
        term.send(b"\x7f" * len("-missing"))
        term.expect("Filter branches")
        term.send("main\r")
        term.expect("Letters, numbers and dashes")
        term.send("worktree-ui\r")
        term.finish()
        workspace = root / ".dynws/workspaces/worktree-ui/git-api"
        assert workspace.is_symlink() and (workspace / ".git").is_file()
    finally:
        term.close()


def shell_tests(root, env, shell):
    path = shutil.which(shell)
    assert path, f"{shell} is required"
    args = [path, "-f"] if shell == "zsh" else [path, "--noprofile", "--norc", "-i"]
    env = dict(env)
    if shell == "zsh":
        unsafe = root / "unsafe-completions"
        unsafe.mkdir()
        unsafe.chmod(0o777)
        env["DWS_TEST_UNSAFE_COMPLETIONS"] = str(unsafe)
    term = Terminal(args, root, env)
    try:
        ready = "printf 'SHELL_%s\\n' READY"
        if shell == "zsh":
            # Hosted runners can have insecure system completion directories.
            # Audit a known unsafe fixture, then ignore unsafe paths rather than
            # prompting (or trusting them with compinit -u).
            term.send("PS1='DWS_PROMPT> '; fpath=(\"$DWS_TEST_UNSAFE_COMPLETIONS\" $fpath); "
                      "autoload -Uz compaudit compinit; dws_insecure_dirs=($(compaudit 2>/dev/null)); "
                      "[[ ${dws_insecure_dirs[(Ie)$DWS_TEST_UNSAFE_COMPLETIONS]} -gt 0 ]] && "
                      "compinit -i -d \"$HOME/zcompdump\" && "
                      "[[ ${fpath[(Ie)$DWS_TEST_UNSAFE_COMPLETIONS]} -eq 0 ]] && " + ready + "\r")
        else:
            term.send("PS1='DWS_PROMPT> '; " + ready + "\r")
        term.expect("SHELL_READY", screen=False)
        hook = f"DWS_COMPLETE={shell} {shlex.quote(str(BINARY))}"
        activation = f"source <({hook})" if shell == "zsh" else f'eval "$({hook})"'
        term.send(f"{activation}; printf 'HOOK_%s\\n' READY\r")
        term.expect("HOOK_READY", screen=False)
        term.read(0.3)
        mark = len(term.output)
        term.send("dws op\t --he\t\r")
        term.expect("Usage: dws open", since=mark)
        # Readline/ZLE executes actual completed input, not a callback-only probe.
        create(root, env, f"fresh-{shell}", "extra")
        mark = len(term.output)
        term.send(f"dws open fresh-{shell[0]}\t\r")
        term.expect("EDITOR_STARTED", since=mark, screen=False)
        term.expect("DWS_PROMPT>", since=mark, screen=False)
        data = json.loads((root / "editor.json").read_text())
        assert data["args"][-1].endswith(f"workspaces/fresh-{shell}"), data
        run(root, env, "edit", f"fresh-{shell}", "--name", f"renamed-{shell}")
        mark = len(term.output)
        term.send(f"dws open renamed-{shell[0]}\t\r")
        term.expect("EDITOR_STARTED", since=mark, screen=False)
        term.expect("DWS_PROMPT>", since=mark, screen=False)
        data = json.loads((root / "editor.json").read_text())
        assert data["args"][-1].endswith(f"workspaces/renamed-{shell}"), data
        run(root, env, "remove", f"renamed-{shell}")
        term.send("exit\r")
        term.finish()
    finally:
        term.close()


def main():
    assert BINARY.is_file(), f"build the dws binary first: {BINARY}"
    with tempfile.TemporaryDirectory(prefix="dws-terminal-") as directory:
        root = Path(directory).resolve()
        env = dict(os.environ, TERM="xterm-256color", NO_COLOR="1", HOME=str(root / "home"), PATH=f"{BINARY.parent}:{os.environ.get('PATH', '')}")
        env.pop("DYNWS_HOME", None)
        env.pop("DWS_COMPLETE", None)
        for name in ["api", "worker", "extra", "home"]:
            (root / name).mkdir()
        (root / "extra/folder-preview.txt").write_text("preview fixture")
        run(root, env, "init")
        create(root, env, "api-review")
        open_with_tests(root, env)
        terminal_tests(root, env)
        mouse_tests(root, env)
        worktree_workflow(root, env)
        for shell in ["zsh", "bash"]:
            shell_tests(root, env, shell)
    print("terminal verification passed")


if __name__ == "__main__":
    main()
