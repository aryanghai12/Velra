"""A full product pass against a locally built release binary.

    cargo build --release -p velra && python scripts/smoke.py

Drives the hook path exactly as Claude Code would - enable, a task with a
failing test and an abandoned edit, the PreCompact barrier, the capsule
coming back at SessionStart(compact), then status, doctor and disable. It
touches nothing outside a temporary directory and never reads or writes the
real Claude Code settings.

This is the automated half of docs/E2E_CHECKLIST.md; the checklist still
covers what only a real Claude Code session can show.
"""
import io, json, os, shutil, subprocess, tempfile
exe = os.path.abspath('target/release/velra.exe')
root = os.path.join(tempfile.gettempdir(), 'velra-e2e')
shutil.rmtree(root, ignore_errors=True)
home, proj, cfg = (os.path.join(root, d) for d in ('home', 'proj', 'claude'))
os.makedirs(os.path.join(proj, 'src')); os.makedirs(cfg)
env = dict(os.environ, VELRA_HOME=home, CLAUDE_CONFIG_DIR=cfg, CLAUDE_PROJECT_DIR=proj,
           VELRA_CLAUDE_VERSION='2.1.268', TZ='UTC')
settings = os.path.join(cfg, 'settings.json')
open(settings, 'w').write('{\n  // my settings\n  "model": "opus"\n}\n')
before = open(settings).read()

def write(text):
    # Bytes exactly as a tool would leave them: no CRLF translation.
    io.open(f, 'w', newline='').write(text)

def cli(*args, stdin=None):
    r = subprocess.run([exe, *args], input=(stdin or '').encode(), capture_output=True, env=env)
    return r.returncode, r.stdout.decode('utf-8', 'replace'), r.stderr.decode('utf-8', 'replace')

def hook(name, payload):
    code, out, err = cli('hook', name, stdin=json.dumps(payload))
    assert code == 0 and not err, (name, code, err)
    return out

print('== enable'); print(cli('enable')[1].strip())
S = {"session_id": "e2e-1", "cwd": proj, "transcript_path": os.path.join(root, 't.jsonl')}
f = os.path.join(proj, 'src', 'auth.py')
ORIGINAL, TRIED = 'max_age = None\n', 'max_age = 0\n'
write(ORIGINAL)

hook('session-start', {**S, "hook_event_name": "SessionStart", "source": "startup"})
hook('user-prompt-submit', {**S, "hook_event_name": "UserPromptSubmit", "prompt_id": "p1",
     "prompt": "fix the flaky logout test and keep the session cookie behaviour intact"})

# PostToolUse fires after the edit is on disk.
write(TRIED)
hook('post-tool-use', {**S, "hook_event_name": "PostToolUse", "tool_name": "Edit", "tool_use_id": "t1",
     "tool_input": {"file_path": f, "old_string": "max_age = None", "new_string": "max_age = 0"},
     "tool_response": {"filePath": f, "originalFile": ORIGINAL}})
hook('post-tool-use', {**S, "hook_event_name": "PostToolUse", "tool_name": "Bash", "tool_use_id": "t2",
     "tool_input": {"command": "pytest tests/test_auth.py -x"},
     "tool_response": {"stdout": 'E   assert response.cookies["session"] is None\ntests/test_auth.py:88: AssertionError\n',
                       "stderr": "", "exit_code": 1, "interrupted": False}})
# The approach is abandoned: the file goes back to what it was.
write(ORIGINAL)
hook('post-tool-use', {**S, "hook_event_name": "PostToolUse", "tool_name": "Edit", "tool_use_id": "t3",
     "tool_input": {"file_path": f, "old_string": "max_age = 0", "new_string": "max_age = None"},
     "tool_response": {"filePath": f, "originalFile": TRIED}})
hook('stop', {**S, "hook_event_name": "Stop"})
cli('reduce', stdin='{}')

print('\n== pre-compact (checkpoint barrier)')
out = hook('pre-compact', {**S, "hook_event_name": "PreCompact", "trigger": "manual", "custom_instructions": None})
print('stdout:', out.strip() or '(empty)')

print('\n== session-start(compact): the capsule comes back')
capsule = json.loads(hook('session-start', {**S, "hook_event_name": "SessionStart", "source": "compact"}))
capsule = capsule["hookSpecificOutput"]["additionalContext"]
print(capsule)
print('dead end recorded:', 'DEAD_ENDS' in capsule)

print('\n== replay: a second channel must not inject twice')
out2 = hook('user-prompt-submit', {**S, "hook_event_name": "UserPromptSubmit", "prompt_id": "p2", "prompt": "continue"})
print('second channel injected:', bool(out2.strip()))

print('\n== inspect --section dead-ends'); print(cli('inspect', '--section', 'dead-ends')[1].strip())
print('\n== status'); print(cli('status')[1].strip())
print('\n== doctor'); code, out, _ = cli('doctor'); print(out.strip()); print('exit', code)
print('\n== disable'); print(cli('disable', '--yes')[1].strip())
print('\nsettings byte-identical after enable+disable:', 'YES' if open(settings).read() == before else 'NO')
