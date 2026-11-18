#!/usr/bin/env python3
"""Packaged Windows/macOS onboarding controller; no external account authorization.

This is intentionally separate from consumer-onboarding-browser.mjs: that test
uses synthetic installation boundaries. This controller verifies the actual
desktop archive and bundled Instance/Slack bytes before the real-browser driver.
The test driver may have Python/Node/Playwright; application children receive an
OS-only PATH. No developer tool is part of the consumer installation.

Package inspection and --self-test are offline checks, not a native onboarding
pass. --run requires exact packages and qualification tools. Slack cleanup uses
its finite checked retirement; Instance uses the existing fixture-owned OS
cleanup with exact profile and process-generation checks, not a product API.
"""
from __future__ import annotations

import argparse
import base64
import ctypes
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import plistlib
import shutil
import stat
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import uuid
import xml.etree.ElementTree as ET
import zipfile


MAX_ARCHIVE_BYTES = 2 * 1024**3
MAX_UNPACKED_BYTES = 3 * 1024**3
MAX_MEMBERS = 100_000
SHA256 = re.compile(r"[0-9a-f]{64}\Z")
LIVE_LIMITS = [
    "Actual browser and local installation controls; no provider/model request",
    "No Slack ticket, workspace consent, app deployment, or first reply",
    "CI account; not clean retail OS, signing/notarization, or accessibility proof",
    "No claim that an API-key canary authenticates to a provider",
]


class FixtureBlocked(RuntimeError):
    """A required supported control is missing; no native dispatch is permitted."""


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as source:
        for part in iter(lambda: source.read(1024 * 1024), b""):
            value.update(part)
    return value.hexdigest()


def ordinary(path: Path, *, directory: bool = False) -> Path:
    info = path.lstat()
    if stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & 0x400:
        raise ValueError("Fixture input must not be a link or reparse point")
    if not (stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode)):
        raise ValueError("Fixture input has the wrong file type")
    return path


def member_path(name: str) -> PurePosixPath:
    # Reject normalization aliases, Windows drive/ADS syntax, and extraction
    # traversal even when inspection is performed on a different platform.
    part = PurePosixPath(name)
    if (not name or "\\" in name or ":" in name or part.is_absolute()
            or any(p in ("", ".", "..") for p in name.split("/"))
            or str(part) != name):
        raise ValueError("Unsafe or aliased archive member")
    return part


def checked_hash(value: str) -> str:
    if not isinstance(value, str) or SHA256.fullmatch(value) is None:
        raise ValueError("Expected an exact lowercase SHA256")
    return value


def snapshot(path: Path) -> dict:
    """Read-only preservation evidence; never repair or traverse foreign links."""
    ordinary(path, directory=path.is_dir())
    info = path.stat()
    value = {"path": str(path), "device": info.st_dev, "inode": info.st_ino,
             "mode": stat.S_IMODE(info.st_mode), "directory": path.is_dir()}
    if path.is_file():
        value.update(size=info.st_size, sha256=digest(path))
    else:
        value["entries"] = sorted(p.name for p in path.iterdir())
    return value


def directory_identity(path: Path) -> dict:
    ordinary(path,directory=True);info=path.stat()
    return {'device':str(info.st_dev),'inode':str(info.st_ino)}


def require_preserved(before: list[dict]) -> list[dict]:
    after = [snapshot(Path(item["path"])) for item in before]
    if after != before:
        raise AssertionError("A retained sentinel identity, inventory, or byte digest changed")
    return after


def inspect_package(metadata_path: Path, expected_metadata_sha256: str) -> dict:
    ordinary(metadata_path)
    if digest(metadata_path) != checked_hash(expected_metadata_sha256):
        raise ValueError("The frozen package metadata changed")
    record = json.loads(metadata_path.read_bytes())
    if (record.get("schema") != 1 or record.get("component") != "agent-observatory"
            or record.get("os") not in ("windows", "macos")
            or any(record.get(key) is not True for key in
                   ("includes_agent_setup", "includes_computer_setup", "includes_slack_setup"))):
        raise FixtureBlocked("Require a complete Windows/macOS desktop with Instance and Slack support")
    name = member_path(record["filename"])
    if len(name.parts) != 1 or not name.name.endswith(".zip"):
        raise ValueError("Use the frozen normal desktop ZIP")
    archive = ordinary(metadata_path.parent / name.name)
    if archive.stat().st_size > MAX_ARCHIVE_BYTES or digest(archive) != checked_hash(record["sha256"]):
        raise ValueError("Desktop archive size or SHA256 differs")
    entry = member_path(record["entry"])
    root = entry.parent
    with zipfile.ZipFile(archive) as source:
        infos = source.infolist()
        if len(infos) > MAX_MEMBERS or sum(i.file_size for i in infos) > MAX_UNPACKED_BYTES:
            raise ValueError("Desktop package exceeds the bounded fixture inventory")
        seen = set()
        for info in infos:
            relative = member_path(info.filename)
            if str(relative).casefold() in seen:
                raise ValueError("Duplicate or case-aliased package member")
            seen.add(str(relative).casefold())
            mode = info.external_attr >> 16
            if info.is_dir() or (stat.S_IFMT(mode) not in (0, stat.S_IFREG)) or info.flag_bits & 1:
                raise ValueError("Package contains a nonordinary or encrypted member")
        def read(name: PurePosixPath, limit: int = 8 * 1024**2) -> bytes:
            info = source.getinfo(str(name))
            if info.file_size > limit:
                raise ValueError("Excessive package metadata")
            return source.read(info)
        def member_digest(name: PurePosixPath) -> str:
            result = hashlib.sha256()
            with source.open(str(name)) as incoming:
                for block in iter(lambda: incoming.read(1024 * 1024), b""):
                    result.update(block)
            return result.hexdigest()
        binary_sha = member_digest(entry)
        if binary_sha != checked_hash(record["binary_sha256"]):
            raise ValueError("Packaged Observatory executable differs")
        package = json.loads(read(root / "observatory-package.json"))
        if any(package.get(key) != record.get(key) for key in ("version", "target", "binary_sha256", "slack_bundle")):
            raise ValueError("Desktop metadata and embedded package provenance differ")
        instance_root = root / "support/instance"
        instance_bytes = read(instance_root / "consumer-bundle.json")
        if hashlib.sha256(instance_bytes).hexdigest() != checked_hash(package["instance_manifest_sha256"]):
            raise ValueError("Bundled Instance manifest differs")
        instance = json.loads(instance_bytes)
        if (instance.get("kind") != "instance-consumer-bundle" or instance.get("target") != record["target"]
                or instance.get("version") != record["instance_version"] or not instance.get("native_runtime")):
            raise ValueError("Bundled Instance target/version/engine binding differs")
        for relative, expected in instance["files"].items():
            member_path(relative)
            actual = member_digest(instance_root / relative)
            if actual != checked_hash(expected):
                raise ValueError("Bundled Instance inventory differs: " + relative)
        slack = package["slack_bundle"]
        slack_root = root / "support/slack"
        descriptor_bytes = read(slack_root / "descriptor.json")
        if hashlib.sha256(descriptor_bytes).hexdigest() != checked_hash(slack["descriptor_sha256"]):
            raise ValueError("Bundled Slack descriptor differs")
        descriptor = json.loads(descriptor_bytes)
        if any(descriptor.get(k) != slack.get(k) for k in
               ("source", "version", "target", "archive_size", "archive_sha256")):
            raise ValueError("Bundled Slack archive descriptor differs")
        embedded = slack_root / (record["target"] + ".zip")
        if (source.getinfo(str(embedded)).file_size != slack["archive_size"]
                or member_digest(embedded) != checked_hash(slack["archive_sha256"])):
            raise ValueError("Bundled Slack archive differs")
    return {"record": record, "archive": str(archive), "package": package,
            "instance": instance, "slack": descriptor,
            "metadata_sha256": expected_metadata_sha256}


def require_live_owner_contract() -> None:
    # Ordinary Slack controls are frozen. The Instance cleanup below remains
    # fixture-owned, not a newly advertised product retirement API.
    if not callable(checked_owned_cleanup):
        raise FixtureBlocked("Required exact owned-profile cleanup is unavailable")


def verify_tool(path: Path, expected: str) -> str:
    if not path.is_absolute() or digest(ordinary(path)) != checked_hash(expected):
        raise ValueError("Use the already supplied exact qualification tool")
    return str(path)


def os_only_environment() -> dict[str, str]:
    # Copy OS login/location fields, not the developer shell's credential store.
    allowed = {"SYSTEMROOT", "WINDIR", "USERPROFILE", "HOMEDRIVE", "HOMEPATH",
               "APPDATA", "LOCALAPPDATA", "TEMP", "TMP", "TMPDIR", "USERNAME",
               "USERDOMAIN", "USER", "LOGNAME", "HOME", "LANG", "LC_ALL", "LC_CTYPE",
               "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS"}
    env = {key: value for key, value in os.environ.items() if key.upper() in allowed}
    if os.name == "nt":
        root = Path(os.environ["SystemRoot"])
        env["PATH"] = os.pathsep.join(map(str,(root / "System32", root,
                                                root / "System32/WindowsPowerShell/v1.0")))
    else:
        env["PATH"] = "/usr/bin:/bin:/usr/sbin:/sbin"
    return env


class ConsoleMonitor:
    """Bounded visible-console sampling, not a universal no-flash claim."""
    def __init__(self):
        self.stop_event=threading.Event();self.thread=None;self.error=None
        self.baseline=[];self.unexpected=[];self.samples=0

    @staticmethod
    def windows():
        from ctypes import wintypes as w
        user=ctypes.WinDLL('user32',use_last_error=True)
        callback_type=ctypes.WINFUNCTYPE(w.BOOL,w.HWND,w.LPARAM)
        user.EnumWindows.argtypes=[callback_type,w.LPARAM];user.EnumWindows.restype=w.BOOL
        user.IsWindowVisible.argtypes=[w.HWND];user.IsWindowVisible.restype=w.BOOL
        user.GetClassNameW.argtypes=[w.HWND,w.LPWSTR,ctypes.c_int]
        user.GetWindowThreadProcessId.argtypes=[w.HWND,ctypes.POINTER(w.DWORD)]
        rows=[]
        @callback_type
        def observe(window,_):
            if user.IsWindowVisible(window):
                name=ctypes.create_unicode_buffer(256);user.GetClassNameW(window,name,len(name))
                if name.value in {'ConsoleWindowClass','CASCADIA_HOSTING_WINDOW_CLASS'}:
                    pid=w.DWORD();user.GetWindowThreadProcessId(window,ctypes.byref(pid))
                    rows.append({'window':int(window),'pid':pid.value,'class':name.value})
            return True
        if not user.EnumWindows(observe,0):raise ctypes.WinError(ctypes.get_last_error())
        return sorted(rows,key=lambda row:row['window'])

    def start(self):
        if os.name!='nt':return
        self.baseline=self.windows()
        def sample():
            try:
                while not self.stop_event.wait(.05):
                    self.samples+=1
                    for row in self.windows():
                        if row not in self.baseline and row not in self.unexpected and len(self.unexpected)<32:self.unexpected.append(row)
            except BaseException as error:self.error=type(error).__name__
        self.thread=threading.Thread(target=sample,daemon=True);self.thread.start()

    def finish(self):
        self.stop_event.set()
        if self.thread:self.thread.join(timeout=2)
        return {'applicable':os.name=='nt','sample_interval_ms':50,'samples':self.samples,
                'baseline_visible_consoles':self.baseline,'new_visible_consoles':self.unexpected,
                'error':self.error or ('observer_did_not_stop' if self.thread and self.thread.is_alive() else None),
                'scope':'Visible console/window snapshots during actual packaged journey; sub-sample flashes and unrelated ambient UI remain outside attribution.'}


def run_packaged(args, inspected: dict) -> None:
    # Must precede all creation, launch, manager mutation, and browser dispatch.
    require_live_owner_contract()
    if os.name != "nt" and sys.platform != "darwin":
        raise FixtureBlocked("The live fixture is only for Windows and macOS")
    expected_os = "windows" if os.name == "nt" else "macos"
    if inspected["record"]["os"] != expected_os:
        raise ValueError("Package belongs to another operating system")
    node = verify_tool(args.node, args.node_sha256)
    playwright = verify_tool(args.playwright_module, args.playwright_sha256)
    browser = verify_tool(args.browser, args.browser_sha256)
    before = [snapshot(p.absolute()) for p in args.preserve]
    # This directory is deliberately not TemporaryDirectory: any failure retains
    # all evidence and partial homes. Checked cleanup is only a successful phase.
    root = Path(tempfile.mkdtemp(prefix="Observatory actual onboarding café ")).resolve()
    evidence = args.evidence.resolve()
    evidence.mkdir(parents=True, exist_ok=False)
    extracted = root / "download"
    extracted.mkdir()
    with zipfile.ZipFile(inspected["archive"]) as source:
        for info in source.infolist():
            target = extracted.joinpath(*member_path(info.filename).parts)
            target.parent.mkdir(parents=True, exist_ok=True)
            with source.open(info) as incoming, target.open("xb") as output:
                for block in iter(lambda: incoming.read(1024 * 1024), b""):
                    output.write(block)
            target.chmod((info.external_attr >> 16) & 0o777 or 0o644)
    binary = extracted.joinpath(*member_path(inspected["record"]["entry"]).parts)
    if digest(binary) != inspected["record"]["binary_sha256"]:
        raise AssertionError("Transport extraction changed the application")
    sentinel = root / "retained-sentinel"
    sentinel.mkdir()
    (sentinel / "unchanged.txt").write_text("existing installation sentinel; not a real enrolled home\n")
    before += [snapshot(sentinel), snapshot(sentinel / "unchanged.txt")]
    inputs = {"schema": 1, "root": str(root), "home": str(root / "Observatory 日本"),
              "binary": str(binary), "binary_sha256": inspected["record"]["binary_sha256"],
              "evidence": str(evidence), "playwright_module": playwright, "browser": browser,
              "instance": inspected["instance"], "slack": inspected["slack"],
              "operation_id": str(uuid.uuid4()), "limits": LIVE_LIMITS,
              "application_env": os_only_environment()}
    input_path = root / "browser-inputs.json"
    with input_path.open("x", encoding="utf-8") as output:
        json.dump(inputs, output)
    if os.name != "nt": input_path.chmod(0o600)
    result = {"schema": 1, "status": "running", "root": str(root), "before": before,
              "package": inspected["record"], "limits": LIVE_LIMITS}
    outcome = evidence / "completion.json"
    outcome.write_text(json.dumps(result, indent=2) + "\n")
    console=ConsoleMonitor()
    try:
        console.start()
        driver = Path(__file__).with_name("consumer-setup-live-browser.mjs")
        controller = subprocess.Popen([node, str(driver), str(input_path)],
                                      env=os_only_environment(), stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                      creationflags=0x08000000 if os.name == "nt" else 0)
        result["browser_controller_pid"] = controller.pid
        try:
            stdout, stderr = controller.communicate(timeout=900)
        except subprocess.TimeoutExpired:
            # Tell the fixture to stop before any subsequent browser mutation.
            # Do not kill a controller mid-dispatch and then erase its children.
            (root / "fixture-abort").write_text("outer observer deadline; no later action\n")
            raise
        (evidence / "browser.stdout").write_bytes(stdout)
        (evidence / "browser.stderr").write_bytes(stderr)
        if controller.returncode != 0:
            raise AssertionError("Actual browser journey failed; partial homes are retained")
        result["browser"] = json.loads((evidence / "browser-result.json").read_text())
        if result["browser"].get("status") != "local_setup_ready":
            raise AssertionError("Browser did not prove the complete local handoff")
        result['console_observation']=console.finish()
        if console.error or console.unexpected or console.thread and (console.thread.is_alive() or console.samples==0):
            raise AssertionError('The actual journey has an unavailable observer or a new visible console')
        # Filled only with the exact owner contract, never generic raw removal.
        result["cleanup"] = {"state":"started", "audit":str(evidence/"owned-cleanup.json")}
        outcome.write_text(json.dumps(result, indent=2) + "\n")
        result["cleanup"] = checked_owned_cleanup(result["browser"], evidence)
        result["after"] = require_preserved(before)
        result["status"] = "passed"
    except BaseException as error:
        result["status"] = "failed_or_unknown_retained"
        result["error_type"] = type(error).__name__
        retain_cleanup_outcome(result, evidence)
        try: result["after"] = require_preserved(before)
        except Exception as changed: result["preservation_error"] = type(changed).__name__
        raise
    finally:
        result.setdefault('console_observation',console.finish())
        outcome.write_text(json.dumps(result, indent=2) + "\n")


def os_command(argv, *, timeout=30):
    return subprocess.run([str(a) for a in argv], stdin=subprocess.DEVNULL,
                          capture_output=True, timeout=timeout, env=os_only_environment(),
                          creationflags=0x08000000 if os.name == "nt" else 0)


def powershell(script: str, data):
    # Fixed source, separately encoded private fixture inputs; no shell expansion.
    payload = base64.b64encode(json.dumps(data).encode()).decode()
    source = ("$ErrorActionPreference='Stop';$ProgressPreference='SilentlyContinue';"
              "[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false);"
              "$d=ConvertFrom-Json ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('"
              + payload + "')));" + script)
    tool = Path(os.environ["SystemRoot"]) / "System32/WindowsPowerShell/v1.0/powershell.exe"
    result = os_command([tool, "-NoProfile", "-NonInteractive", "-EncodedCommand",
                         base64.b64encode(source.encode("utf-16-le")).decode()])
    if result.returncode:
        raise AssertionError("The exact owned-profile OS observation/action was refused")
    return json.loads(result.stdout.decode("utf-8-sig"))


def windows_arguments(value: str) -> list[str]:
    from ctypes import wintypes as w
    shell = ctypes.WinDLL("shell32", use_last_error=True)
    shell.CommandLineToArgvW.argtypes = [w.LPCWSTR, ctypes.POINTER(ctypes.c_int)]
    shell.CommandLineToArgvW.restype = ctypes.POINTER(w.LPWSTR)
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.LocalFree.argtypes = [w.HLOCAL]; kernel.LocalFree.restype = w.HLOCAL
    count = ctypes.c_int(); argv = shell.CommandLineToArgvW(value, ctypes.byref(count))
    if not argv: raise ctypes.WinError(ctypes.get_last_error())
    try: return [argv[i] for i in range(count.value)]
    finally: kernel.LocalFree(argv)


def path_equal(first: str | Path, second: str | Path) -> bool:
    # Resolve both typed paths and compare canonical spelling AND identity. Windows may
    # report one spelling through DOS and another through the extended prefix;
    # string equality alone is not an ownership proof. A different hardlink,
    # copy or basename is not equal; the resolved sealed interpreter symlink is.
    a,b=Path(first).resolve(strict=True),Path(second).resolve(strict=True)
    def key(path):
        value=str(path)
        if os.name=='nt':
            if value.startswith('\\\\?\\UNC\\'): value='\\\\'+value[8:]
            elif value.startswith('\\\\?\\'): value=value[4:]
            value=os.path.normcase(value)
        return value
    return key(a)==key(b) and os.path.samefile(a,b)


def arguments_equal(actual: list[str], expected: list[str], path_indices: set[int]) -> bool:
    if len(actual) != len(expected): return False
    return all(path_equal(a, b) if index in path_indices else a == b
               for index, (a, b) in enumerate(zip(actual, expected)))


def retain_cleanup_outcome(result: dict, evidence: Path) -> None:
    if 'cleanup' not in result:
        result['cleanup'] = {'state':'not_attempted'}
    elif isinstance(result['cleanup'],dict) and result['cleanup'].get('state') == 'started':
        result['cleanup'] = {'state':'failed_or_unknown_after_start',
                             'audit':str(evidence/'owned-cleanup.json')}


class OwnedProcess:
    """Observer handle/generation only; never grants a kill or replacement."""
    def __init__(self, pid: int, executable: Path):
        if type(pid) is not int or pid <= 0: raise AssertionError("Missing exact owned PID")
        self.pid = pid; self.handle = None
        if os.name == "nt":
            from ctypes import wintypes as w
            self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
            self.kernel.OpenProcess.argtypes = [w.DWORD, w.BOOL, w.DWORD]; self.kernel.OpenProcess.restype = w.HANDLE
            self.kernel.QueryFullProcessImageNameW.argtypes = [w.HANDLE,w.DWORD,w.LPWSTR,ctypes.POINTER(w.DWORD)]
            self.kernel.GetProcessTimes.argtypes = [w.HANDLE,*([ctypes.POINTER(w.FILETIME)]*4)]
            self.kernel.WaitForSingleObject.argtypes = [w.HANDLE,w.DWORD]
            self.kernel.CloseHandle.argtypes = [w.HANDLE]
            self.handle = self.kernel.OpenProcess(0x100000 | 0x1000, False, pid)
            if not self.handle: raise ctypes.WinError(ctypes.get_last_error())
            name = ctypes.create_unicode_buffer(32768); size = w.DWORD(32768)
            if not self.kernel.QueryFullProcessImageNameW(self.handle,0,name,ctypes.byref(size)):
                raise ctypes.WinError(ctypes.get_last_error())
            if not path_equal(name.value,executable): raise AssertionError("Owned executable changed")
            created, exited, kernel, user = (w.FILETIME() for _ in range(4))
            if not self.kernel.GetProcessTimes(self.handle,*map(ctypes.byref,(created,exited,kernel,user))):
                raise ctypes.WinError(ctypes.get_last_error())
            self.identity = {"pid":pid,"creation_time_100ns":str((created.dwHighDateTime << 32) | created.dwLowDateTime)}
            if self.kernel.WaitForSingleObject(self.handle,0) != 258:
                raise AssertionError("Expected owned generation is no longer running")
        else:
            self.identity = self.mac_identity(pid)
            if self.identity is None: raise AssertionError("Expected owned generation is absent")
            lib = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
            lib.proc_pidpath.argtypes = [ctypes.c_int,ctypes.c_void_p,ctypes.c_uint32]
            name = ctypes.create_string_buffer(4096)
            if lib.proc_pidpath(pid,name,len(name)) <= 0: raise OSError("Process executable unavailable")
            if not path_equal(os.fsdecode(name.value),executable) or self.mac_identity(pid) != self.identity:
                raise AssertionError("Owned process identity changed")

    @staticmethod
    def mac_identity(pid):
        c = ctypes
        class Info(c.Structure):
            _fields_ = [(n,c.c_uint32) for n in ('flags','status','xstatus','pid','ppid','uid','gid','ruid','rgid','svuid','svgid','rfu')] + [('comm',c.c_char*16),('name',c.c_char*32)] + [(n,c.c_uint32) for n in ('nfiles','pgid','pjobc','e_tdev','e_tpgid')] + [('nice',c.c_int32),('seconds',c.c_uint64),('microseconds',c.c_uint64)]
        lib = c.CDLL('/usr/lib/libproc.dylib',use_errno=True)
        lib.proc_pidinfo.argtypes = [c.c_int,c.c_int,c.c_uint64,c.c_void_p,c.c_int]
        value = Info(); n = lib.proc_pidinfo(pid,3,0,c.byref(value),c.sizeof(value))
        if n == 0 and c.get_errno() == 3: return None
        if n != c.sizeof(value) or value.pid != pid: raise OSError('Process generation unavailable')
        if value.status == 5: return None
        return {'pid':pid,'start_seconds':str(value.seconds),'start_microseconds':str(value.microseconds)}

    def absent(self):
        if os.name == 'nt':
            result = self.kernel.WaitForSingleObject(self.handle,0)
            if result not in (0,258): raise OSError('Process exit observation unavailable')
            return result == 0
        return self.mac_identity(self.pid) != self.identity

    def close(self):
        if self.handle: self.kernel.CloseHandle(self.handle); self.handle = None


def task_profile(name: str):
    return powershell("$t=Get-ScheduledTask -TaskName $d[0] -TaskPath '\\' -ErrorAction SilentlyContinue;"
                      "if($null -eq $t){@{absent=$true}|ConvertTo-Json -Compress;exit};"
                      "if($t.Principal.UserId -like 'S-1-*'){$sid=$t.Principal.UserId}else{$sid=([Security.Principal.NTAccount]$t.Principal.UserId).Translate([Security.Principal.SecurityIdentifier]).Value};"
                      "@{absent=$false;xml=(Export-ScheduledTask -TaskName $d[0] -TaskPath '\\');"
                      "owner_sid=$sid;current_sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value} | ConvertTo-Json -Compress", [name])


def checked_owned_cleanup(browser_result: dict, evidence: Path) -> dict:
    """Exact new fixture registrations only; Instance has no consumer retire CLI."""
    slack = browser_result['slack_cleanup']; bound = browser_result['instance_cleanup']
    if slack['retired']['state'] != 'retired' or slack['retired']['control_current'] is not True:
        raise AssertionError('Slack checked retirement is not proved')
    home = ordinary(Path(bound['home']),directory=True)
    if directory_identity(home)!=bound['home_identity']: raise AssertionError('Instance home identity changed')
    identifier = str(uuid.UUID(bound['instance_id']))
    if home.name != browser_result['endpoint_id'] or not path_equal(home,browser_result['instance_home']):
        raise AssertionError('New fixture home changed')
    install_path = ordinary(home/'install.json')
    if digest(install_path) != bound['install_sha256']: raise AssertionError('Installation metadata changed')
    install = json.loads(install_path.read_bytes()); config = json.loads((home/'config.json').read_bytes())['config']
    if install['instance_id'] != identifier or config['instance_id'] != identifier:
        raise AssertionError('Fixture installation identity differs')
    binary = ordinary(Path(bound['binary']))
    if not path_equal(binary.parent,home/'bin') or digest(binary) != bound['binary_sha256']:
        raise AssertionError('Installed executable changed')
    helper = ordinary(Path.home()/'.local/share/constellation-updates'/identifier,directory=True)
    helper_identity=directory_identity(helper)
    registration = ordinary(helper/'registration.json'); registration_sha = digest(registration)
    registered = json.loads(registration.read_bytes())
    if registered['instance_id'] != identifier or not path_equal(registered['home'],home) or not path_equal(registered['state'],helper):
        raise AssertionError('Helper registration does not own this exact fixture')
    private = install['private_python']; python = ordinary(home/'support'/private['directory']/(private['windowless_executable'] if os.name=='nt' else private['executable']))
    helper_args = ['-I','-X','utf8',str(helper/'worker.py'),'--registration',str(registration)]
    app_args = ['serve','--home',str(home)] + (['--log-directory',str(home)] if os.name=='nt' else [])
    jobs = []; processes = []; audit = {'schema':1,'instance_id':identifier,'home':str(home),'helper':str(helper),
                                      'registration_sha256':registration_sha,'jobs':jobs,'processes':[],
                                      'scope':'Disposable fixture-owned OS cleanup; Instance exposes no consumer retirement command'}
    audit_path = evidence/'owned-cleanup.json'
    def save(): audit_path.write_text(json.dumps(audit,indent=2)+'\n')
    try:
        for role,exe,argv in [('instance',binary,app_args),('helper',python,helper_args)]:
            path_indices = {2,4} if role=='instance' and os.name=='nt' else {2} if role=='instance' else {3,5}
            if os.name == 'nt':
                name = ('AgentInstance-' if role=='instance' else 'constellation-updates-')+identifier
                profile = task_profile(name)
                if profile['absent'] or profile['owner_sid'] != profile['current_sid']: raise AssertionError('Task owner changed')
                xml = profile['xml']; tree = ET.fromstring(xml); ns={'t':'http://schemas.microsoft.com/windows/2004/02/mit/task'}
                actions = tree.findall('t:Actions/t:Exec',ns)
                if len(actions)!=1 or tree.findtext('t:Principals/t:Principal/t:RunLevel',namespaces=ns)!='LeastPrivilege':
                    raise AssertionError('Task action or privilege changed')
                if not path_equal(actions[0].findtext('t:Command',namespaces=ns),exe): raise AssertionError('Task executable changed')
                if not arguments_equal(windows_arguments('fixture '+(actions[0].findtext('t:Arguments',default='',namespaces=ns)))[1:],argv,path_indices):
                    raise AssertionError('Task arguments changed')
                if role == 'instance': pid = bound['pid']
                else:
                    rows = powershell("@(Get-CimInstance Win32_Process | Where-Object {$_.Name -eq $d[0]} | Select-Object ProcessId,ExecutablePath,CommandLine) | ConvertTo-Json -Compress",[exe.name])
                    if isinstance(rows,dict): rows=[rows]
                    matching=[]
                    for row in rows or []:
                        if not row.get('ExecutablePath') or not row.get('CommandLine'): continue
                        actual=windows_arguments(row['CommandLine'])
                        if (path_equal(row['ExecutablePath'],exe) and actual and path_equal(actual[0],exe)
                                and arguments_equal(actual[1:],argv,path_indices)):
                            matching.append(row)
                    if len(matching)!=1: raise AssertionError('Exact helper process is unavailable or ambiguous')
                    pid=int(matching[0]['ProcessId'])
                job={'role':role,'name':name,'profile':profile,'profile_sha256':hashlib.sha256(xml.encode()).hexdigest()}
            else:
                name=('io.agent-instance.' if role=='instance' else 'io.constellation-updates-')+identifier
                file=ordinary(Path.home()/'Library/LaunchAgents'/(name+'.plist')); raw=file.read_bytes(); profile=plistlib.loads(raw)
                actual=profile.get('ProgramArguments')
                if profile.get('Label')!=name or not isinstance(actual,list) or len(actual)!=len(argv)+1 or not path_equal(actual[0],exe) or not arguments_equal(actual[1:],argv,path_indices):
                    raise AssertionError('LaunchAgent profile changed')
                service=f'gui/{os.getuid()}/{name}'; found=os_command(['/bin/launchctl','print',service])
                match=re.search(rb'(?m)^\s*pid = (\d+)\s*$',found.stdout)
                if found.returncode or not match: raise AssertionError('Exact LaunchAgent PID unavailable')
                pid=int(match[1])
                if role=='instance' and pid!=bound['pid']: raise AssertionError('Instance generation changed before cleanup')
                job={'role':role,'name':name,'service':service,'file':str(file),'profile_sha256':hashlib.sha256(raw).hexdigest()}
            process=OwnedProcess(pid,exe);processes.append(process);job['process_identity']=process.identity;jobs.append(job)
        audit['processes']=[p.identity for p in processes]; save()
        # Preserve the exact observed registration/profile until each stop. No
        # service query is used as permission to stop a different generation.
        for job,process in reversed(list(zip(jobs,processes))):
            if digest(registration)!=registration_sha or process.absent(): raise AssertionError('Owner changed before stop')
            if os.name=='nt':
                if task_profile(job['name'])!=job['profile']: raise AssertionError('Task definition changed before stop')
                job['stop_dispatch_outcome']='intent_or_unknown';save()
                powershell("Stop-ScheduledTask -TaskName $d[0] -TaskPath '\\';@{accepted=$true}|ConvertTo-Json -Compress",[job['name']])
            else:
                if digest(Path(job['file']))!=job['profile_sha256']: raise AssertionError('LaunchAgent changed before stop')
                job['stop_dispatch_outcome']='intent_or_unknown';save()
                stopped=os_command(['/bin/launchctl','bootout',job['service']])
                if stopped.returncode: raise AssertionError('Exact LaunchAgent stop was not acknowledged')
            job['stop_dispatched']=True;job['stop_dispatch_outcome']='acknowledged';save()
        deadline=time.monotonic()+20
        while not all(p.absent() for p in processes):
            if time.monotonic()>=deadline: raise AssertionError('Same owned process generation remains alive')
            time.sleep(.1)
        audit['same_generations_absent']=True;save()
        for job in jobs:
            if os.name=='nt':
                if task_profile(job['name'])!=job['profile']: raise AssertionError('Task definition changed before retirement')
                job['retirement_dispatch_outcome']='intent_or_unknown';save()
                powershell("Unregister-ScheduledTask -TaskName $d[0] -TaskPath '\\' -Confirm:$false;@{accepted=$true}|ConvertTo-Json -Compress",[job['name']])
                if task_profile(job['name'])['absent'] is not True: raise AssertionError('Task absence unconfirmed')
            else:
                inactive=os_command(['/bin/launchctl','print',job['service']])
                if inactive.returncode==0 or not any(s in inactive.stderr for s in (b'Could not find service',b'No such process')): raise AssertionError('LaunchAgent absence unconfirmed')
                if digest(Path(job['file']))!=job['profile_sha256']: raise AssertionError('LaunchAgent file changed')
                job['retirement_dispatch_outcome']='intent_or_unknown';save()
                Path(job['file']).unlink()
            job['registration_absent']=True;job['retirement_dispatch_outcome']='absence_confirmed';save()
        if digest(registration)!=registration_sha: raise AssertionError('Helper registration changed')
        if directory_identity(home)!=bound['home_identity'] or directory_identity(helper)!=helper_identity:
            raise AssertionError('An owned directory identity changed')
        slack_home=ordinary(Path(slack['home']),directory=True)
        if (directory_identity(slack_home)!=slack['home_identity']
                or slack_home.name!='observatory-'+str(uuid.UUID(slack['operation_id']))
                or json.loads((slack_home/'native-installation.json').read_bytes())['id']!=slack['id']
                or digest(slack_home/'adapter.json')!=slack['config_sha256']
                or digest(Path(slack['executable']))!=slack['sha256']):
            raise AssertionError('Retired Slack fixture identity changed')
        # Retirement is complete. Removal is one strict attempt, never a retry
        # over an unknown process, a foreign home, or an old installation.
        audit['homes_removed']=[]
        for owned in (helper,home,slack_home):
            audit['removal_intent_or_unknown']=str(owned);save()
            shutil.rmtree(owned);audit['homes_removed'].append(str(owned));audit.pop('removal_intent_or_unknown');save()
        audit.update(passed=True,homes_removed=[str(helper),str(home),str(slack_home)],slack_retirement=slack['retired']);save()
        return audit
    except BaseException as error:
        audit.update(passed=False,error_type=type(error).__name__,unknowns_retained=True);save();raise
    finally:
        for process in processes: process.close()


class OfflineTests(unittest.TestCase):
    def package_fixture(self, root: Path, *, slack_ready: bool = True) -> Path:
        def sha(value): return hashlib.sha256(value).hexdigest()
        target = "x86_64-pc-windows-msvc"
        files = {"agent-observatory.exe": b"inert Observatory fixture, never executed"}
        instance_binary = b"inert Instance fixture, never executed"
        instance = {"schema": 1, "kind": "instance-consumer-bundle", "version": "0.22.13",
                    "target": target, "binary": "agent-instance.exe", "native_runtime": {"version": "0.15.12"},
                    "files": {"agent-instance.exe": sha(instance_binary)}}
        instance_bytes = json.dumps(instance).encode()
        files["support/instance/consumer-bundle.json"] = instance_bytes
        files["support/instance/agent-instance.exe"] = instance_binary
        archive_bytes = b"inert closed Slack archive; not extracted by this test"
        descriptor = {"schema": 1, "source": "1" * 40, "version": "0.0.0", "target": target,
                      "archive_size": len(archive_bytes), "archive_sha256": sha(archive_bytes)}
        descriptor_bytes = json.dumps(descriptor).encode()
        binding = {**descriptor, "descriptor_sha256": sha(descriptor_bytes)}
        files["support/slack/descriptor.json"] = descriptor_bytes
        files["support/slack/" + target + ".zip"] = archive_bytes
        package = {"schema": 1, "version": "0.0.0", "target": target,
                   "binary_sha256": sha(files["agent-observatory.exe"]), "slack_bundle": binding,
                   "instance_manifest_sha256": sha(instance_bytes)}
        files["observatory-package.json"] = json.dumps(package).encode()
        archive = root / "fixture.zip"
        with zipfile.ZipFile(archive, "w") as zipped:
            for name, data in files.items():
                info = zipfile.ZipInfo(name); info.external_attr = (stat.S_IFREG | 0o644) << 16
                zipped.writestr(info, data)
        record = {"schema": 1, "component": "agent-observatory", "version": "0.0.0", "target": target,
                  "os": "windows", "filename": archive.name, "entry": "agent-observatory.exe",
                  "sha256": digest(archive), "binary_sha256": package["binary_sha256"],
                  "includes_agent_setup": True, "includes_computer_setup": True,
                  "includes_slack_setup": slack_ready, "slack_bundle": binding, "instance_version": instance["version"]}
        metadata = root / "fixture.json"; metadata.write_text(json.dumps(record))
        return metadata

    def test_checked_cleanup_contract_is_required_before_native_dispatch(self):
        from unittest.mock import patch
        require_live_owner_contract()
        with patch(__name__+'.checked_owned_cleanup',None),self.assertRaises(FixtureBlocked):require_live_owner_contract()

    def test_directory_identity_does_not_equate_a_replacement(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);original=root/'owned';original.mkdir();before=directory_identity(original)
            original.rename(root/'retained');original.mkdir()
            self.assertNotEqual(directory_identity(original),before)

    def test_path_comparison_requires_same_file_not_equal_contents_or_basename(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);file=root/'python';file.write_bytes(b'identical')
            copy=root/'other';copy.mkdir();other=copy/'python';other.write_bytes(b'identical')
            self.assertTrue(path_equal(file,file))
            self.assertFalse(path_equal(file,other))
            hardlink=root/'another-name';os.link(file,hardlink)
            self.assertFalse(path_equal(file,hardlink))
            if os.name=='nt':
                self.assertTrue(path_equal(str(file),'\\\\?\\'+str(file.resolve())))
            else:
                alias=root/'alias';alias.symlink_to(file);self.assertTrue(path_equal(alias,file))

    def test_only_typed_argument_paths_are_normalized_flags_and_order_are_exact(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);home=root/'home';home.mkdir()
            expected=['serve','--home',str(home)]
            actual=['serve','--home',str(home/'.')]
            self.assertTrue(arguments_equal(actual,expected,{2}))
            for changed in [['serve','--HOME',str(home)],['--home','serve',str(home)],expected+['--extra']]:
                self.assertFalse(arguments_equal(changed,expected,{2}))
            other=root/'copy';other.mkdir()
            self.assertFalse(arguments_equal(['serve','--home',str(other)],expected,{2}))

    def test_partial_cleanup_is_not_relabelled_unattempted_and_success_is_retained(self):
        evidence=Path('/fixture-evidence');result={}
        retain_cleanup_outcome(result,evidence)
        self.assertEqual(result['cleanup'],{'state':'not_attempted'})
        result['cleanup']={'state':'started','audit':str(evidence/'owned-cleanup.json')}
        retain_cleanup_outcome(result,evidence)
        self.assertEqual(result['cleanup'],{'state':'failed_or_unknown_after_start','audit':str(evidence/'owned-cleanup.json')})
        succeeded={'passed':True,'homes_removed':['owned']};result['cleanup']=succeeded
        retain_cleanup_outcome(result,evidence)
        self.assertIs(result['cleanup'],succeeded)

    def test_unconfirmed_slack_retirement_refuses_before_any_instance_cleanup(self):
        from unittest.mock import patch
        for value in [{'state':'unknown','control_current':True},{'state':'retired','control_current':False}]:
            with patch(__name__+'.os_command') as command,patch(__name__+'.powershell') as ps,self.assertRaises(AssertionError):
                checked_owned_cleanup({'slack_cleanup':{'retired':value},'instance_cleanup':{}},Path('.'))
            command.assert_not_called();ps.assert_not_called()

    def test_member_aliases_are_refused_on_every_platform(self):
        for value in ("../a", "/a", "a/../b", "a//b", "./a", "C:/a", "a\\b", "a:", "a/"):
            with self.subTest(value=value), self.assertRaises(ValueError): member_path(value)
        self.assertEqual(str(member_path("Observatory.app/Contents/MacOS/agent-observatory")),
                         "Observatory.app/Contents/MacOS/agent-observatory")

    def test_preservation_checks_content_and_inventory(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); file = root / "sentinel"; file.write_bytes(b"original")
            before = [snapshot(root), snapshot(file)]
            self.assertEqual(require_preserved(before), before)
            file.write_bytes(b"replaced")
            with self.assertRaises(AssertionError): require_preserved(before)

    def test_nonempty_exact_hash_only(self):
        self.assertEqual(checked_hash("a" * 64), "a" * 64)
        for value in ("", "A" * 64, "a" * 63, "a" * 64 + "\n"):
            with self.assertRaises(ValueError): checked_hash(value)

    def test_exact_nested_packages_are_required_without_running_them(self):
        with tempfile.TemporaryDirectory() as temporary:
            metadata = self.package_fixture(Path(temporary))
            inspected = inspect_package(metadata, digest(metadata))
            self.assertEqual(inspected["instance"]["version"], "0.22.13")
            self.assertEqual(inspected["slack"]["archive_sha256"], inspected["package"]["slack_bundle"]["archive_sha256"])

    def test_missing_slack_fails_instead_of_skipping_native_coverage(self):
        with tempfile.TemporaryDirectory() as temporary:
            metadata = self.package_fixture(Path(temporary), slack_ready=False)
            with self.assertRaises(FixtureBlocked): inspect_package(metadata, digest(metadata))

    def test_resealed_outer_archive_cannot_hide_changed_instance_payload(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); metadata = self.package_fixture(root)
            archive = root / "fixture.zip"
            with zipfile.ZipFile(archive) as source:
                files = [(info, source.read(info)) for info in source.infolist()]
            with zipfile.ZipFile(archive, "w") as output:
                for info, value in files:
                    if info.filename == "support/instance/agent-instance.exe": value = b"changed"
                    output.writestr(info, value)
            record = json.loads(metadata.read_text()); record["sha256"] = digest(archive)
            metadata.write_text(json.dumps(record))
            with self.assertRaisesRegex(ValueError, "Instance inventory differs"):
                inspect_package(metadata, digest(metadata))

    def test_os_environment_drops_ambient_credentials(self):
        original = dict(os.environ)
        try:
            os.environ.update(OPENAI_API_KEY="do-not-copy", SLACK_BOT_TOKEN="do-not-copy", NODE_OPTIONS="bad")
            env = os_only_environment()
            self.assertNotIn("OPENAI_API_KEY", env)
            self.assertNotIn("SLACK_BOT_TOKEN", env)
            self.assertNotIn("NODE_OPTIONS", env)
        finally:
            os.environ.clear(); os.environ.update(original)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--metadata", type=Path)
    parser.add_argument("--metadata-sha256")
    parser.add_argument("--run", action="store_true", help="Requires the frozen owner cleanup contract")
    parser.add_argument("--node", type=Path); parser.add_argument("--node-sha256")
    parser.add_argument("--playwright-module", type=Path); parser.add_argument("--playwright-sha256")
    parser.add_argument("--browser", type=Path); parser.add_argument("--browser-sha256")
    parser.add_argument("--evidence", type=Path)
    parser.add_argument("--preserve", type=Path, action="append", default=[])
    args = parser.parse_args()
    if args.self_test:
        result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(OfflineTests))
        raise SystemExit(0 if result.wasSuccessful() else 1)
    if not args.metadata or not args.metadata_sha256:
        parser.error("--metadata and --metadata-sha256 bind the existing packaged artifact")
    inspected = inspect_package(args.metadata.resolve(strict=True), args.metadata_sha256)
    if args.run:
        run_packaged(args, inspected)
    else:
        print(json.dumps({"status": "offline_package_verified", "native_test": "not run",
                          "metadata_sha256": inspected["metadata_sha256"],
                          "package": inspected["record"], "limits": LIVE_LIMITS,
                          "remaining_gate": "Exact packaged live browser/manager qualification has not run"}, indent=2))


if __name__ == "__main__":
    main()
