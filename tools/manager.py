#!/usr/bin/env python3
"""Manage Evelyn's pinned Linux host tools."""

import argparse
import base64
import difflib
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import secrets
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import tomllib
from urllib.error import HTTPError
from urllib.request import Request, urlopen
from urllib.parse import quote, unquote, urljoin, urlsplit
import uuid
import xml.etree.ElementTree as ET
import zipfile

BASE = Path(__file__).resolve().parent
ROOT = BASE / ".cache"
MANIFEST = BASE / "manifest.toml"


def setup_signing_keys():
    directory = BASE / "keys"
    keystore = directory / "evelyn.p12"
    password = directory / "password"
    if shutil.which("keytool") is None:
        raise ValueError("Signing key setup needs keytool from JDK 11+")
    if directory.exists() or directory.is_symlink():
        if directory.is_symlink() or not directory.is_dir() or any(
            path.is_symlink() or not path.is_file() for path in (keystore, password)
        ):
            raise ValueError("Existing tools/keys/ is incomplete or contains links; restore your signing keys. It was not replaced.")
        subprocess.run(["keytool", "-list", "-keystore", str(keystore), "-storetype", "PKCS12",
                        "-storepass:file", str(password), "-alias", "evelyn"],
                       check=True, stdout=subprocess.DEVNULL)
        directory.chmod(0o700)
        keystore.chmod(0o600)
        password.chmod(0o600)
        print("Using existing signing keys in tools/keys/")
        return

    # Keep staged secrets ignored, and publish the complete identity atomically.
    with tempfile.TemporaryDirectory(prefix=".keys-", dir=BASE) as temporary:
        stage = Path(temporary)
        secret = stage / "password"
        secret.write_text(secrets.token_hex(32) + "\n")
        secret.chmod(0o600)
        subprocess.run(["keytool", "-genkeypair", "-keystore", str(stage / "evelyn.p12"),
                        "-storetype", "PKCS12", "-alias", "evelyn", "-keyalg", "RSA",
                        "-keysize", "4096", "-validity", "10000", "-dname", "CN=Evelyn APK Signing",
                        "-storepass:file", str(secret)], check=True)
        (stage / "evelyn.p12").chmod(0o600)
        stage.rename(directory)
    print("Created signing keys in tools/keys/. Back up this directory to retain your APK signing identity.")


def digest(path):
    checksum = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            checksum.update(block)
    return checksum.hexdigest()


def fingerprint(package):
    return hashlib.sha256(json.dumps(package, sort_keys=True).encode()).hexdigest()


def relative(path):
    if not isinstance(path, str) or not path or "\\" in path or PurePosixPath(path).is_absolute() or any(
        part in ("", ".", "..") for part in path.split("/")
    ):
        raise ValueError(f"Unsafe relative path: {path!r}")
    return path


def fields(value, required, optional, context):
    if not isinstance(value, dict):
        raise ValueError(f"{context}: expected a table")
    missing = set(required) - value.keys()
    unknown = value.keys() - set(required) - set(optional)
    if missing or unknown:
        raise ValueError(f"{context}: missing fields {sorted(missing)}; unknown fields {sorted(unknown)}")


def string(value, context):
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{context}: expected a nonempty string")
    return value


def https(value):
    parsed = urlsplit(string(value, "URL"))
    if parsed.scheme != "https" or not parsed.hostname or parsed.username is not None or any(c.isspace() for c in value):
        raise ValueError("URL must use HTTPS, include a host and contain no credentials or whitespace")


def validate_manifest(manifest):
    fields(manifest, ["schema", "tools"], [], "manifest")
    if type(manifest.get("schema")) is not int or manifest["schema"] != 1:
        raise ValueError("Unsupported tool manifest schema")
    if not isinstance(manifest["tools"], dict):
        raise ValueError("tools must contain named tool tables")
    packages, links = [], set()
    for name, tool in manifest["tools"].items():
        if not re.fullmatch(r"[a-z0-9-]+", name):
            raise ValueError(f"Invalid tool name: {name}")
        fields(tool, ["version", "source", "downloads"], ["update", "license"], name)
        if not re.fullmatch(r"[A-Za-z0-9.+_-]+", string(tool["version"], name + ".version")):
            raise ValueError(f"Invalid version for {name}")
        https(tool["source"])
        package = {"name": name, "version": tool["version"], "source": tool["source"], "assets": [], "links": {}}
        paths = set()
        if not isinstance(tool["downloads"], list) or not tool["downloads"]:
            raise ValueError(f"{name}: downloads must be a nonempty array")
        for entry in tool["downloads"]:
            fields(entry, ["url", "sha256", "format", "files"], [], name + ".downloads")
            https(entry["url"])
            if not re.fullmatch(r"[0-9a-f]{64}", string(entry["sha256"], name + ".sha256")):
                raise ValueError(f"Invalid SHA-256: {name}")
            if entry["format"] not in ("raw", "base64", "zip", "tar"):
                raise ValueError(f"Unsupported archive format: {name}")
            if not isinstance(entry["files"], list) or not entry["files"]:
                raise ValueError(f"{name}: files must be a nonempty array")
            if entry["format"] in ("raw", "base64") and len(entry["files"]) != 1:
                raise ValueError(f"{name}: raw downloads need exactly one file")
            asset = {key: entry[key] for key in ("url", "sha256", "format")}
            asset["files"] = {}
            for spec in entry["files"]:
                fields(spec, ["path"], ["install", "executable"], name + ".files")
                path = relative(spec["path"])
                if path.split("/")[0] == ".receipt.json" or any(
                    path == previous or path.startswith(previous + "/") or previous.startswith(path + "/")
                    for previous in paths
                ):
                    raise ValueError(f"Duplicate or reserved installed path: {path}")
                paths.add(path)
                executable = spec.get("executable", False)
                if not isinstance(executable, bool):
                    raise ValueError(f"{name}: executable must be true or false")
                member = "" if entry["format"] in ("raw", "base64") else path
                asset["files"][member] = {"path": path, "executable": executable}
                if "install" in spec:
                    destination = relative(spec["install"])
                    if destination.split("/")[0] not in ("bin", "scripts") or "/" not in destination or any(
                        destination == previous or destination.startswith(previous + "/") or previous.startswith(destination + "/")
                        for previous in links
                    ):
                        raise ValueError(f"Invalid or duplicate public path: {destination}")
                    links.add(destination)
                    package["links"][destination] = path
            package["assets"].append(asset)
        if "update" in tool:
            upstream = tool["update"]
            fields(upstream, ["kind"], ["repo", "ref", "url", "prefix"], name + ".update")
            kind = upstream["kind"]
            expected = {"github": ["kind", "repo"], "github_commit": ["kind", "repo", "ref"],
                        "gitiles": ["kind", "url"], "sdk": ["kind", "url", "prefix"]}
            if not isinstance(kind, str) or kind not in expected:
                raise ValueError(f"{name}: unsupported update kind: {kind}")
            fields(upstream, expected[kind], [], name + ".update")
            for key, value in upstream.items():
                string(value, name + ".update." + key)
            if "repo" in upstream and not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", upstream["repo"]):
                raise ValueError(f"{name}: GitHub repo must be owner/repository")
            if "url" in upstream:
                https(upstream["url"])
            package["update"] = dict(upstream)
        if "license" in tool:
            license = tool["license"]
            fields(license, ["id", "sha256", "url"], [], name + ".license")
            https(license["url"])
            string(license["id"], name + ".license.id")
            if not re.fullmatch(r"[0-9a-f]{64}", string(license["sha256"], name + ".license.sha256")):
                raise ValueError(f"Invalid upstream SDK license reference: {name}")
            package["license"] = dict(license)
        packages.append(package)
    return packages


def load_manifest():
    return validate_manifest(tomllib.loads(MANIFEST.read_text()))


def response(url):
    for attempt in range(3):
        try:
            stream = urlopen(Request(url, headers={"User-Agent": "evelyn-tool-fetcher"}), timeout=30)
            break
        except HTTPError as error:
            if attempt == 2 or (error.code != 429 and error.code < 500):
                raise
            print(f"Upstream returned HTTP {error.code}; retrying {url}", file=sys.stderr, flush=True)
            error.close()
            time.sleep(attempt + 1)
    if not stream.geturl().startswith("https://"):
        stream.close()
        raise ValueError("Refusing download redirected away from HTTPS")
    return stream


def download(asset):
    cache = ROOT / "downloads"
    cache.mkdir(parents=True, exist_ok=True)
    checksum = asset.get("sha256")
    target = cache / checksum if checksum else None
    if target is not None and target.is_file() and digest(target) == checksum:
        return target
    with tempfile.TemporaryDirectory(prefix=".download-", dir=cache) as temporary:
        pending = Path(temporary) / "payload"
        with response(asset["url"]) as source, pending.open("wb") as output:
            shutil.copyfileobj(source, output)
        if asset["format"] == "base64":
            pending.write_bytes(base64.b64decode(b"".join(pending.read_bytes().split()), validate=True))
        actual = digest(pending)
        if checksum and actual != checksum:
            raise ValueError(f"SHA-256 mismatch: {asset['url']}")
        target = cache / actual
        os.replace(pending, target)
    return target


def archive_members(payload, format):
    if format == "zip":
        with zipfile.ZipFile(payload) as archive:
            entries = [(member.filename.rstrip("/"), member.is_dir(), member.external_attr >> 16)
                       for member in archive.infolist()]
            for path, directory, mode in entries:
                if stat.S_IFMT(mode) not in (0, stat.S_IFDIR if directory else stat.S_IFREG):
                    raise ValueError(f"Archive link or special file is not allowed: {path}")
    elif format == "tar":
        with tarfile.open(payload) as archive:
            entries = []
            for member in archive.getmembers():
                if not (member.isfile() or member.isdir()):
                    raise ValueError(f"Archive link or special file is not allowed: {member.name}")
                entries.append((member.name.rstrip("/"), member.isdir(), member.mode))
    else:
        return []
    names = set()
    files = []
    for path, directory, mode in entries:
        relative(path)
        if path in names:
            raise ValueError(f"Duplicate archive member: {path}")
        names.add(path)
        if not directory:
            files.append({"path": path, "executable": bool(mode & 0o111)})
    return files


def unpack(asset, payload, destination):
    def write(source, spec):
        target = destination / relative(spec["path"])
        target.parent.mkdir(parents=True, exist_ok=True)
        with target.open("xb") as output:
            shutil.copyfileobj(source, output)
        target.chmod(0o755 if spec.get("executable") else 0o644)

    files = asset["files"]
    archive_members(payload, asset["format"])
    if asset["format"] in ("raw", "base64"):
        if set(files) != {""}:
            raise ValueError("Raw downloads must declare exactly one output")
        with payload.open("rb") as source:
            write(source, files[""])
    elif asset["format"] == "zip":
        with zipfile.ZipFile(payload) as archive:
            for member, spec in files.items():
                with archive.open(member) as source:
                    write(source, spec)
    else:
        with tarfile.open(payload) as archive:
            for member, spec in files.items():
                source = archive.extractfile(member)
                if source is None:
                    raise ValueError(f"Not an archive file: {member}")
                with source:
                    write(source, spec)


def replace_link(path, target):
    if path.exists() and not path.is_symlink():
        raise ValueError(f"Refusing to overwrite a non-link: {path}")
    path.parent.mkdir(parents=True, exist_ok=True)
    pending = path.with_name(f".{path.name}-{uuid.uuid4().hex}")
    try:
        pending.symlink_to(target)
        os.replace(pending, path)
    finally:
        pending.unlink(missing_ok=True)


def installed(package):
    current = ROOT / "packages" / package["name"] / "current"
    try:
        if not current.is_symlink() or current.resolve().parent != current.parent.resolve():
            return False
        receipt = json.loads((current / ".receipt.json").read_text())
        if receipt["fingerprint"] != fingerprint(package):
            return False
        expected = {spec["path"]: spec for asset in package["assets"] for spec in asset["files"].values()}
        if receipt["files"].keys() != expected.keys():
            return False
        for path, spec in expected.items():
            file = current / path
            if file.is_symlink() or not file.is_file() or digest(file) != receipt["files"][path]:
                return False
            if stat.S_IMODE(file.stat().st_mode) != (0o755 if spec.get("executable") else 0o644):
                return False
        for destination, path in package["links"].items():
            link = BASE / destination
            target = os.path.relpath(current / path, link.parent)
            if not link.is_symlink() or os.readlink(link) != target:
                return False
        return True
    except (OSError, ValueError, KeyError):
        return False


def accept_license(package):
    license = package.get("license")
    if not license:
        return
    marker = ROOT / "licenses" / license["sha256"]
    if marker.is_file() and marker.read_text() == license["sha256"]:
        return
    with response(license["url"]) as source:
        metadata = ET.parse(source).getroot()
    terms = next((element.text for element in metadata if element.attrib.get("id") == license["id"]), None)
    if terms is None or hashlib.sha256((terms + "\n").encode()).hexdigest() != license["sha256"]:
        raise ValueError("Upstream SDK terms changed; review the license pin before installing")
    print(terms)
    try:
        accepted = input(f"Accept this license for {package['name']}? [yes/NO] ") == "yes"
    except EOFError:
        accepted = False
    if not accepted:
        raise ValueError("SDK license was not accepted; existing tools were preserved")
    marker.parent.mkdir(parents=True, exist_ok=True)
    marker.write_text(license["sha256"])


def install(package):
    if installed(package):
        print(f"Verified {package['name']} {package['version']}")
        return
    accept_license(package)
    parent = ROOT / "packages" / package["name"]
    parent.mkdir(parents=True, exist_ok=True)
    # Check command collisions before changing the active package.
    for destination in package["links"]:
        path = BASE / destination
        if path.exists() and not path.is_symlink():
            raise ValueError(f"Refusing to overwrite a non-link: {path}")
    with tempfile.TemporaryDirectory(prefix=".stage-", dir=parent) as temporary:
        stage = Path(temporary) / "install"
        stage.mkdir()
        for asset in package["assets"]:
            unpack(asset, download(asset), stage)
        files = {spec["path"]: digest(stage / spec["path"])
                 for asset in package["assets"] for spec in asset["files"].values()}
        (stage / ".receipt.json").write_text(json.dumps({"fingerprint": fingerprint(package), "files": files}))
        generation = parent / f"{package['version']}-{uuid.uuid4().hex[:12]}"
        stage.rename(generation)
        replace_link(parent / "current", generation.name)
        for destination, path in package["links"].items():
            link = BASE / destination
            replace_link(link, os.path.relpath(parent / "current" / path, link.parent))
    if not installed(package):
        raise ValueError(f"Installed package did not verify: {package['name']}")
    print(f"Installed {package['name']} {package['version']}")


def latest(package):
    upstream = package["update"]
    kind = upstream["kind"]
    if kind == "github":
        url = f"https://api.github.com/repos/{upstream['repo']}/releases/latest"
    elif kind == "github_commit":
        url = f"https://api.github.com/repos/{upstream['repo']}/git/ref/{upstream['ref']}"
    elif kind in ("sdk", "gitiles"):
        url = upstream["url"]
    else:
        raise ValueError(f"Unknown update source: {kind}")
    with response(url) as source:
        data = source.read()
    if kind == "sdk":
        versions = []
        for item in ET.fromstring(data):
            path = item.attrib.get("path", "")
            channel = item.find("channelRef")
            if (path.startswith(upstream["prefix"]) and channel is not None
                    and channel.attrib.get("ref") == "channel-0"
                    and item.find("revision/preview") is None):
                version = path[len(upstream["prefix"]):]
                if re.fullmatch(r"\d+\.\d+\.\d+", version):
                    versions.append(version)
        if not versions:
            raise ValueError("No stable SDK version in upstream metadata")
        version = max(versions, key=lambda value: tuple(map(int, value.split("."))))
    else:
        metadata = json.loads(data.decode().removeprefix(")]}'\n"))
        if kind == "github":
            if metadata.get("draft") or metadata.get("prerelease"):
                raise ValueError("Latest release is not stable")
            version = metadata["tag_name"]
        elif kind == "github_commit":
            version = metadata["object"]["sha"]
        else:
            version = metadata["commit"]
    if not isinstance(version, str) or not version:
        raise ValueError("Missing upstream version")
    return version


def check_updates(packages):
    failed = False
    for package in packages:
        try:
            if "update" not in package:
                print(f"{package['name']}: no update source configured", flush=True)
                continue
            available = latest(package)
            status = "current" if available == package["version"] else f"available {available}"
            print(f"{package['name']}: pinned {package['version']}; {status} ({package['source']})", flush=True)
        except (OSError, ValueError, KeyError, TypeError, ET.ParseError) as error:
            print(f"{package['name']}: update check failed: {error}", file=sys.stderr, flush=True)
            failed = True
    return int(failed)


def ask(label, default="", choices=None, required=False):
    while True:
        value = input(f"{label}" + (f" [{default}]" if default != "" else "") + ": ").strip()
        value = "" if value == "-" else value or str(default)
        if required and not value:
            print("A value is required.")
        elif choices and value not in choices:
            print("Choose one of: " + ", ".join(choices))
        else:
            return value


def yes(label, default=False):
    return ask(label, "yes" if default else "no", ["yes", "no"]) == "yes"


def configure_update(existing):
    kind = ask("Update source", existing.get("kind", "none"),
               ["none", "github", "github_commit", "gitiles", "sdk"])
    if kind == "none":
        return None
    result = {"kind": kind}
    keys = {"github": ["repo"], "github_commit": ["repo", "ref"],
            "gitiles": ["url"], "sdk": ["url", "prefix"]}[kind]
    for key in keys:
        result[key] = ask(f"Update {key}", existing.get(key, ""), required=True)
    return result


def configure_download(existing=None, asset=None, suggested_url=None):
    existing = existing or {}
    url = asset["browser_download_url"] if asset else ask("Download URL", suggested_url or existing.get("url", ""), required=True)
    https(url)
    filename = unquote(PurePosixPath(urlsplit(url).path).name)
    guessed = "zip" if filename.endswith(".zip") else "tar" if ".tar" in filename or filename.endswith((".tgz", ".txz", ".tbz2")) else "raw"
    format = ask("Format", guessed if asset else existing.get("format", guessed),
                 ["raw", "base64", "zip", "tar"])
    unchanged = url == existing.get("url") and format == existing.get("format")
    published = asset.get("digest") if asset else None
    if published and published.startswith("sha256:"):
        checksum = published.removeprefix("sha256:")
        print(f"Download: {url}\nGitHub asset SHA-256: {checksum}")
    else:
        checksum = ask("Expected SHA-256 (optional; - clears)", existing.get("sha256", "") if unchanged else "")
    if checksum and not re.fullmatch(r"[0-9a-f]{64}", checksum):
        raise ValueError("Expected SHA-256 must be 64 lowercase hexadecimal characters")
    entry = {"url": url, "format": format}
    if checksum:
        entry["sha256"] = checksum
    payload = download(entry)
    entry["sha256"] = digest(payload)
    print(f"SHA-256: {entry['sha256']}" + (" (verified)" if checksum else " (calculated; review provenance)"))
    members = archive_members(payload, format)
    previous = existing.get("files", [])
    if format in ("raw", "base64"):
        path = ask("Downloaded filename", previous[0]["path"] if previous else filename, required=True)
        selected = [{"path": relative(path), "executable": False}]
    else:
        if not members:
            raise ValueError("Archive contains no files")
        for number, member in enumerate(members, 1):
            print(f"  {number}: {member['path']}")
        defaults = [str(number) for number, member in enumerate(members, 1)
                    if any(spec["path"] == member["path"] for spec in previous)]
        if len(members) == 1:
            defaults = ["1"]
        while True:
            choice = ask("Files to keep (comma-separated numbers or *)", ",".join(defaults), required=True)
            try:
                numbers = list(range(1, len(members) + 1)) if choice == "*" else list(dict.fromkeys(int(v.strip()) for v in choice.split(",")))
                if not numbers or any(n < 1 or n > len(members) for n in numbers):
                    raise ValueError("Invalid file number")
                selected = [members[n - 1] for n in numbers]
                break
            except ValueError:
                print("Choose file numbers from the list.")
    files = []
    for member in selected:
        path = member["path"]
        old = next((spec for spec in previous if spec["path"] == path), None)
        if old is None:
            matches = [spec for spec in previous if PurePosixPath(spec["path"]).name == PurePosixPath(path).name]
            old = matches[0] if len(matches) == 1 else {}
        print(f"Configure {path}")
        destination = ask("Public path relative to tools/ (blank keeps private; - clears)", old.get("install", ""))
        spec = {"path": path}
        if destination:
            spec["install"] = relative(destination)
        executable = old.get("executable", False) if old else member["executable"]
        if yes("Executable", executable):
            spec["executable"] = True
        files.append(spec)
    entry["files"] = files
    return entry


def github_release(repo):
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo):
        raise ValueError("GitHub repo must be owner/repository")
    url = f"https://api.github.com/repos/{repo}/releases"
    with response(url + "?per_page=20") as source:
        releases = json.load(source)
    stable = [release for release in releases if not release.get("draft") and not release.get("prerelease")]
    if not stable:
        raise ValueError(f"No stable releases found for {repo}; use a direct URL instead")
    for release in stable:
        print(f"  {release['tag_name']}: {release.get('name') or release['tag_name']}")
    tag = ask("Release tag", stable[0]["tag_name"], required=True)
    release = next((release for release in stable if release["tag_name"] == tag), None)
    if release is None:
        with response(url + "/tags/" + quote(tag, safe="")) as source:
            release = json.load(source)
    if release.get("draft") or release.get("prerelease"):
        raise ValueError("Choose a published stable release")
    return release


def github_downloads(release, previous):
    assets = release["assets"]
    if not assets:
        raise ValueError("Release has no downloadable assets")
    defaults = []
    for number, asset in enumerate(assets, 1):
        print(f"  {number}: {asset['name']} ({asset['size']} bytes)")
        if any(unquote(PurePosixPath(urlsplit(entry["url"]).path).name) == asset["name"] for entry in previous):
            defaults.append(str(number))
    if len(assets) == 1:
        defaults = ["1"]
    while True:
        choice = ask("Assets to download (comma-separated numbers)", ",".join(defaults), required=True)
        try:
            numbers = list(dict.fromkeys(int(value.strip()) for value in choice.split(",")))
            if not numbers or any(n < 1 or n > len(assets) for n in numbers):
                raise ValueError("Invalid asset number")
            break
        except ValueError:
            print("Choose asset numbers from the list.")
    downloads = []
    for index, number in enumerate(numbers):
        asset = assets[number - 1]
        old = next((entry for entry in previous if entry["url"] == asset["browser_download_url"]), None)
        if old is None:
            old = previous[index] if index < len(previous) else {}
        downloads.append(configure_download(old, asset=asset))
    return downloads


def configure_tool(existing=None):
    tool = dict(existing or {})
    if existing:
        tool["version"] = ask("Pinned version or commit", tool["version"], required=True)
    tool["source"] = ask("Upstream project URL", tool.get("source", ""), required=True)
    if not existing or yes("Change update source"):
        github = re.fullmatch(r"https://github.com/([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)/?", tool["source"])
        default = {"kind": "github", "repo": github[1]} if github else {}
        update = configure_update(tool.get("update", default))
        if update:
            tool["update"] = update
        else:
            tool.pop("update", None)
    if not existing and tool.get("update", {}).get("kind") != "github":
        tool["version"] = ask("Pinned version or commit", required=True)
    if not existing or yes("Change downloads or file selection"):
        if tool.get("update", {}).get("kind") == "github":
            release = github_release(tool["update"]["repo"])
            tool["version"] = release["tag_name"]
            downloads = github_downloads(release, tool.get("downloads", []))
        else:
            downloads = [configure_download(entry) for entry in tool.get("downloads", [{}])]
        while yes("Add another download"):
            downloads.append(configure_download())
        tool["downloads"] = downloads
    if yes("Configure SDK license acceptance metadata"):
        license = tool.get("license", {})
        tool["license"] = {key: ask(f"License {key}", license.get(key, ""), required=True)
                           for key in ("id", "url", "sha256")}
    elif "license" in tool and yes("Remove existing SDK license requirement"):
        del tool["license"]
    return tool


def update_tool(name, tool, package):
    upstream = tool.get("update")
    if upstream is None:
        print("No update source configured; enter the new pin and download details.")
        return configure_tool(tool)
    if upstream["kind"] == "github":
        print(f"Pinned release: {tool['version']}")
        release = github_release(upstream["repo"])
        version = release["tag_name"]
    else:
        version = latest(package)
    if version == tool["version"]:
        print(f"{name}: already pinned to {version}")
        return tool
    print(f"Updating {name}: {tool['version']} -> {version}")
    updated = dict(tool, version=version)
    if upstream["kind"] == "github":
        updated["downloads"] = github_downloads(release, tool["downloads"])
    elif upstream["kind"] == "sdk":
        with response(upstream["url"]) as source:
            metadata = ET.parse(source).getroot()
        remote = next((item for item in metadata if item.attrib.get("path") == upstream["prefix"] + version), None)
        if remote is None:
            raise ValueError("Selected SDK release is missing from upstream metadata")
        archives = [item for item in remote.findall("archives/archive") if item.findtext("host-os") == "linux"]
        if len(archives) != 1 or len(tool["downloads"]) != 1:
            raise ValueError("SDK update needs one Linux archive and one existing download; use edit")
        archive_url = string(archives[0].findtext("complete/url"), "SDK archive URL")
        url = urljoin(upstream["url"], archive_url)
        updated["downloads"] = [configure_download(tool["downloads"][0], suggested_url=url)]
        license_ref = remote.find("uses-license")
        if license_ref is not None:
            identifier = license_ref.attrib["ref"]
            terms = next((item.text for item in metadata if item.attrib.get("id") == identifier), None)
            if terms is None:
                raise ValueError("Selected SDK license is missing from upstream metadata")
            updated["license"] = {"id": identifier, "url": upstream["url"],
                                  "sha256": hashlib.sha256((terms + "\n").encode()).hexdigest()}
    else:
        downloads = []
        for entry in tool["downloads"]:
            if tool["version"] in entry["url"]:
                url = entry["url"].replace(tool["version"], version)
                downloads.append(configure_download(entry, suggested_url=url))
            else:
                print(f"Retaining separately pinned download: {entry['url']}")
                downloads.append(entry)
        if downloads == tool["downloads"]:
            raise ValueError("Download URLs do not contain the pinned revision; use edit to choose new URLs")
        updated["downloads"] = downloads
    return updated


def toml_editor():
    try:
        import tomlkit
    except ImportError as error:
        raise ValueError("Editing needs TOML Kit. Run python3 -m venv tools/.cache/venv, then "
                         "tools/.cache/venv/bin/python -m pip install --require-hashes -r tools/requirements.txt; "
                         "use that Python to run manager.py.") from error
    return tomlkit


def tool_table(tool, kit):
    table = kit.table()
    for key, value in tool.items():
        if key in ("update", "license"):
            item = kit.inline_table()
            item.update(value)
        elif key == "downloads":
            item = kit.aot()
            for entry in value:
                download_table = kit.table()
                for field, setting in entry.items():
                    if field == "files":
                        array = kit.array().multiline(True)
                        for spec in setting:
                            row = kit.inline_table()
                            row.update(spec)
                            array.append(row)
                        download_table[field] = array
                    else:
                        download_table[field] = setting
                item.append(download_table)
        else:
            item = value
        table[key] = item
    return table


def save_manifest(before, document, kit):
    after = kit.dumps(document)
    packages = validate_manifest(tomllib.loads(after))
    if before == after:
        print("No manifest changes.")
        return
    for package in packages:
        for destination in package["links"]:
            path = BASE / destination
            if path.exists() and not path.is_symlink():
                raise ValueError(f"Refusing to replace an existing file or directory: {path}")
    print("".join(difflib.unified_diff(before.splitlines(True), after.splitlines(True),
                                      fromfile="manifest.toml", tofile="manifest.toml")), end="")
    if not yes("Save manifest changes"):
        print("Cancelled; manifest unchanged.")
        return
    if MANIFEST.read_text() != before:
        raise ValueError("Manifest changed during editing; rerun the command")
    pending = MANIFEST.with_name(f".manifest-{uuid.uuid4().hex}.toml")
    try:
        pending.write_text(after)
        pending.chmod(stat.S_IMODE(MANIFEST.stat().st_mode))
        os.replace(pending, MANIFEST)
    finally:
        pending.unlink(missing_ok=True)
    public = {destination for package in packages for destination in package["links"]}
    for package in validate_manifest(tomllib.loads(before)):
        for destination, path in package["links"].items():
            link = BASE / destination
            target = os.path.relpath(ROOT / "packages" / package["name"] / "current" / path, link.parent)
            if destination not in public and link.is_symlink() and os.readlink(link) == target:
                link.unlink()
    print("Saved manifest. Review the Git diff; run manager.py fetch to install changed pins.")


def manage_entry(command, name):
    kit = toml_editor()
    before = MANIFEST.read_text()
    document = kit.parse(before)
    tools = tomllib.loads(before)["tools"]
    if command == "add":
        name = name or ask("Tool name", required=True)
        if not re.fullmatch(r"[a-z0-9-]+", name):
            raise ValueError("Tool names may contain lowercase letters, digits and hyphens")
        if name in tools:
            raise ValueError(f"Tool already exists: {name}; use edit")
        document["tools"][name] = tool_table(configure_tool(), kit)
    else:
        if name not in tools:
            raise ValueError(f"Unknown tool: {name}")
        if command == "remove":
            del document["tools"][name]
            if not document["tools"]:
                document["tools"] = kit.table()
        else:
            if command == "update":
                package = next(package for package in validate_manifest(tomllib.loads(before)) if package["name"] == name)
                updated = update_tool(name, tools[name], package)
            else:
                updated = configure_tool(tools[name])
            table = document["tools"][name]
            replacement = tool_table(updated, kit)
            for key in list(table):
                if key not in updated:
                    del table[key]
            for key, value in updated.items():
                if value != tools[name].get(key):
                    table[key] = replacement[key]
    save_manifest(before, document, kit)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for name, help in [("fetch", "install or repair pinned tools"), ("check", "verify installed pins offline"),
                       ("list", "show pinned versions and installation status"),
                       ("outdated", "report upstream versions without changing pins")]:
        command = commands.add_parser(name, help=help)
        command.add_argument("names", nargs="*", help="tool names (default: all)")
        if name == "fetch":
            command.description = "Install pinned tools. A full fetch also initializes local signing keys once; existing keys are reused."
    for name in ("add", "edit", "remove", "update"):
        command = commands.add_parser(name, help=f"{name} a manifest entry with diff review")
        command.add_argument("name", nargs="?" if name == "add" else None)
    args = parser.parse_args(argv)
    try:
        if platform.system() != "Linux" or platform.machine() != "x86_64":
            raise ValueError("Only Linux x86-64 hosts are supported")
        packages = load_manifest()
        if args.command in ("add", "edit", "remove", "update"):
            manage_entry(args.command, args.name)
            return 0
        unknown = set(args.names) - {package["name"] for package in packages}
        if unknown:
            raise ValueError("Unknown tools: " + ", ".join(sorted(unknown)))
        packages = [package for package in packages if not args.names or package["name"] in args.names]
        if args.command == "outdated":
            return check_updates(packages)
        if args.command == "list":
            for package in packages:
                status = "installed" if installed(package) else "missing or mismatched"
                print(f"{package['name']:<25} {package['version']:<42} {status}")
        elif args.command == "check":
            missing = [package["name"] for package in packages if not installed(package)]
            if missing:
                raise ValueError("Missing or mismatched tools: " + ", ".join(missing) +
                                 ". Run ./tools/manager.py fetch before building.")
            print(f"Verified {len(packages)} tool(s)")
        else:
            for package in packages:
                install(package)
            if not args.names:
                setup_signing_keys()
        return 0
    except (OSError, ValueError, KeyError, EOFError, subprocess.CalledProcessError,
            ET.ParseError, tarfile.TarError, zipfile.BadZipFile) as error:
        print(f"Tool manager: {error}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("\nCancelled.", file=sys.stderr)
        return 130


if __name__ == "__main__":
    sys.exit(main())
