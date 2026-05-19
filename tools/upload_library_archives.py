#!/usr/bin/env python3
"""Upload library ZIP archives на R2 + Cloud.ru mirrors.

Per `tools/LIBRARY-ARCHIVES-CONTRACT.md` (cross-team coordination doc в
AR Hud repo), MDM team отвечает за mirror upload тематических ZIP-архивов
которые KB team builds локально.

Usage:
    # Single archive
    python3 tools/upload_library_archives.py path/to/medical-tactical-v1.zip

    # Directory с несколькими (typical use-case после KB team batch-build)
    python3 tools/upload_library_archives.py /path/to/build/library-archives/

    # Dry-run — только compute sha256 + sizes, без upload'а
    python3 tools/upload_library_archives.py --dry-run path/to/archives/

Mirror key prefix: `library/archives/`. Имя файла на mirror'е = basename
локального файла. Sha256 sidecar (`<name>.sha256`) загружается рядом
для AR Hud LibraryDownloader integrity-verify до распаковки.

Credentials читаются из tactical-ar-hud `.tmp/r2-creds.env` и
`.tmp/cloudru-creds.env` — те же что для APK/V25/models. Если переменные
заданы в process env, можно поменять путь через --creds-dir.

ВАЖНО: Cloud.ru endpoint требует NO_PROXY bypass на Windows-host через
Hide.My.Name VPN (xray не маршрутизирует .cloud.ru). Script автоматически
добавляет нужные suffix'ы в NO_PROXY.
"""
from __future__ import annotations

import argparse
import hashlib
import os
import sys
import time
from pathlib import Path
from typing import Optional
from urllib.parse import urlparse

import boto3
from botocore.config import Config

DEFAULT_CREDS = Path(r"F:\projects\tactical-ar-hud\.tmp")
MIRROR_PREFIX = "library/archives/"


def load_env(p: Path) -> dict:
    out = {}
    for line in p.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, v = line.split("=", 1)
        out[k.strip()] = v.strip()
    return out


def compute_sha256(path: Path, chunk: int = 8 * 1024 * 1024) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        while True:
            b = f.read(chunk)
            if not b:
                break
            h.update(b)
    return h.hexdigest()


def s3_client_r2(env: dict):
    return boto3.client(
        "s3",
        endpoint_url=env["R2_ENDPOINT_URL"],
        aws_access_key_id=env["R2_ACCESS_KEY_ID"],
        aws_secret_access_key=env["R2_SECRET_ACCESS_KEY"],
        config=Config(signature_version="s3v4"),
        region_name="auto",
    )


def s3_client_cloudru(env: dict):
    access_key = f"{env['CLOUDRU_TENANT_ID']}:{env['CLOUDRU_KEY_ID']}"
    return boto3.client(
        "s3",
        endpoint_url=env["CLOUDRU_ENDPOINT_URL"],
        aws_access_key_id=access_key,
        aws_secret_access_key=env["CLOUDRU_SECRET_ACCESS_KEY"],
        config=Config(signature_version="s3v4", retries={"max_attempts": 5, "mode": "standard"}),
        region_name=env.get("CLOUDRU_REGION", "ru-central-1"),
    )


def setup_no_proxy(cr_env: dict) -> None:
    """Hide.My.Name VPN's xray не маршрутизирует cloud.ru. Bypass обязателен."""
    endpoint_host = urlparse(cr_env["CLOUDRU_ENDPOINT_URL"]).hostname or ""
    prev = os.environ.get("NO_PROXY", "")
    additions = [endpoint_host, ".cloud.ru", ".s3.cloud.ru"]
    items = [x for x in prev.split(",") if x]
    for a in additions:
        if a and a not in items:
            items.append(a)
    new = ",".join(items)
    os.environ["NO_PROXY"] = new
    os.environ["no_proxy"] = new


def upload_to_mirror(label: str, s3, bucket: str, local: Path, key: str) -> int:
    size = local.stat().st_size
    print(f"  [{label}] PUT {key}  ({size:,} bytes)")
    t0 = time.time()
    s3.upload_file(str(local), bucket, key)
    dt = time.time() - t0
    rate = size / 1024 / 1024 / dt if dt else 0
    print(f"  [{label}] OK in {dt:.1f}s ({rate:.2f} MB/s)")
    return size


def verify_head(label: str, s3, bucket: str, key: str, expected_size: int) -> bool:
    r = s3.head_object(Bucket=bucket, Key=key)
    actual = r["ContentLength"]
    ok = actual == expected_size
    mark = "OK" if ok else "MISMATCH"
    print(f"  [{label}] HEAD {key}: size={actual:,} ({mark})")
    return ok


def upload_archive(
    r2_env: dict, cr_env: dict, s3_r2, s3_cr, local: Path, dry_run: bool
) -> bool:
    print(f"\n=== {local.name} ===")
    sha = compute_sha256(local)
    size = local.stat().st_size
    print(f"  size:   {size:,} bytes ({size / 1024 / 1024:.2f} MiB)")
    print(f"  sha256: {sha}")
    if dry_run:
        print("  (dry-run, skipping upload)")
        return True

    key_zip = f"{MIRROR_PREFIX}{local.name}"
    key_sha = key_zip + ".sha256"
    sha_body = f"{sha}  {local.name}\n"

    # Upload ZIP к обоим mirror'ам
    try:
        upload_to_mirror("R2", s3_r2, r2_env["R2_BUCKET"], local, key_zip)
    except Exception as exc:
        print(f"  [R2 FAIL] {exc!r}")
        return False
    try:
        upload_to_mirror("Cloud.ru", s3_cr, cr_env["CLOUDRU_BUCKET"], local, key_zip)
    except Exception as exc:
        print(f"  [Cloud.ru FAIL] {exc!r}")
        return False

    # Upload .sha256 sidecar (inline body, не файл)
    try:
        s3_r2.put_object(
            Bucket=r2_env["R2_BUCKET"],
            Key=key_sha,
            Body=sha_body.encode(),
            ContentType="text/plain; charset=utf-8",
        )
        s3_cr.put_object(
            Bucket=cr_env["CLOUDRU_BUCKET"],
            Key=key_sha,
            Body=sha_body.encode(),
            ContentType="text/plain; charset=utf-8",
        )
        print(f"  [sha256 sidecar] uploaded to both mirrors at {key_sha}")
    except Exception as exc:
        print(f"  [sha256 sidecar FAIL] {exc!r}")
        # Non-fatal — ZIP уже загружен, integrity можно вычислить self.

    # Verify both mirrors
    r2_ok = verify_head("R2", s3_r2, r2_env["R2_BUCKET"], key_zip, size)
    cr_ok = verify_head("Cloud.ru", s3_cr, cr_env["CLOUDRU_BUCKET"], key_zip, size)

    return r2_ok and cr_ok


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("path", help="Path to single ZIP or directory с ZIP'ами")
    parser.add_argument("--dry-run", action="store_true", help="Compute sha + sizes без upload'а")
    parser.add_argument(
        "--creds-dir",
        default=str(DEFAULT_CREDS),
        help=f"Directory с r2-creds.env и cloudru-creds.env (default: {DEFAULT_CREDS})",
    )
    args = parser.parse_args()

    target = Path(args.path)
    if not target.exists():
        print(f"ERROR: path не существует: {target}", file=sys.stderr)
        return 2

    # Collect ZIP files
    if target.is_file():
        if target.suffix.lower() != ".zip":
            print(f"WARN: {target} не .zip", file=sys.stderr)
        archives = [target]
    else:
        archives = sorted(target.glob("*.zip"))
    if not archives:
        print(f"ERROR: ни одного .zip не найдено в {target}", file=sys.stderr)
        return 2
    print(f"Found {len(archives)} archive(s):")
    for a in archives:
        print(f"  - {a.name} ({a.stat().st_size / 1024 / 1024:.1f} MiB)")

    creds_dir = Path(args.creds_dir)
    r2_env = load_env(creds_dir / "r2-creds.env")
    cr_env = load_env(creds_dir / "cloudru-creds.env")
    setup_no_proxy(cr_env)

    s3_r2 = s3_client_r2(r2_env) if not args.dry_run else None
    s3_cr = s3_client_cloudru(cr_env) if not args.dry_run else None

    failures = 0
    for arc in archives:
        if not upload_archive(r2_env, cr_env, s3_r2, s3_cr, arc, args.dry_run):
            failures += 1

    print(f"\n[done] {len(archives) - failures}/{len(archives)} uploaded successfully")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
