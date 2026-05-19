#!/usr/bin/env python3
"""Upload bootstrap-manifest.json to R2 + Cloud.ru roots.

AR Hud's `ManifestLoader.refreshFromNetwork()` берёт manifest по
`<mirror_base>/bootstrap-manifest.json` — то есть **root** bucket'а
без префикса. Этот скрипт кладёт текущий manifest на оба mirror'а.

Usage:
    # Default: берёт из tactical-ar-hud bundled assets.
    python3 tools/upload_manifest.py

    # Explicit path:
    python3 tools/upload_manifest.py /path/to/bootstrap-manifest.json

Idempotent (overwrite). Содержит NO_PROXY bypass для Cloud.ru (Hide.My.Name
VPN'a xray не маршрутизирует .cloud.ru).
"""
from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path
from urllib.parse import urlparse

import boto3
from botocore.config import Config

DEFAULT_MANIFEST = Path(
    r"F:\projects\tactical-ar-hud\prototypes\outpost-android\app\src\main\assets\bootstrap-manifest.json"
)
DEFAULT_CREDS = Path(r"F:\projects\tactical-ar-hud\.tmp")
ROOT_KEY = "bootstrap-manifest.json"  # без префикса — root mirror'а


def load_env(p: Path) -> dict:
    out = {}
    for line in p.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, v = line.split("=", 1)
        out[k.strip()] = v.strip()
    return out


def setup_no_proxy(cr_env: dict) -> None:
    endpoint_host = urlparse(cr_env["CLOUDRU_ENDPOINT_URL"]).hostname or ""
    prev = os.environ.get("NO_PROXY", "")
    items = [x for x in prev.split(",") if x]
    for a in (endpoint_host, ".cloud.ru", ".s3.cloud.ru"):
        if a and a not in items:
            items.append(a)
    new = ",".join(items)
    os.environ["NO_PROXY"] = new
    os.environ["no_proxy"] = new


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument(
        "manifest",
        nargs="?",
        default=str(DEFAULT_MANIFEST),
        help=f"Path to bootstrap-manifest.json (default: {DEFAULT_MANIFEST})",
    )
    parser.add_argument(
        "--creds-dir",
        default=str(DEFAULT_CREDS),
        help=f"Directory с r2-creds.env + cloudru-creds.env (default: {DEFAULT_CREDS})",
    )
    args = parser.parse_args()

    manifest_path = Path(args.manifest)
    if not manifest_path.exists():
        print(f"ERROR: manifest не найден: {manifest_path}", file=sys.stderr)
        return 2
    body = manifest_path.read_text(encoding="utf-8").encode("utf-8")
    print(f"=== Upload {manifest_path.name} ({len(body):,} bytes) ===")

    creds_dir = Path(args.creds_dir)
    r2_env = load_env(creds_dir / "r2-creds.env")
    cr_env = load_env(creds_dir / "cloudru-creds.env")
    setup_no_proxy(cr_env)

    failures = 0

    # R2
    try:
        r2 = boto3.client(
            "s3",
            endpoint_url=r2_env["R2_ENDPOINT_URL"],
            aws_access_key_id=r2_env["R2_ACCESS_KEY_ID"],
            aws_secret_access_key=r2_env["R2_SECRET_ACCESS_KEY"],
            config=Config(signature_version="s3v4"),
            region_name="auto",
        )
        r2.put_object(
            Bucket=r2_env["R2_BUCKET"],
            Key=ROOT_KEY,
            Body=body,
            ContentType="application/json; charset=utf-8",
            CacheControl="public, max-age=300",  # 5 min — manifest updates быстро
        )
        public = r2_env.get("R2_PUBLIC_URL", "").rstrip("/")
        print(f"  [R2] OK → {public}/{ROOT_KEY}")
    except Exception as e:
        print(f"  [R2 FAIL] {e!r}", file=sys.stderr)
        failures += 1

    # Cloud.ru
    try:
        access_key = f"{cr_env['CLOUDRU_TENANT_ID']}:{cr_env['CLOUDRU_KEY_ID']}"
        cr = boto3.client(
            "s3",
            endpoint_url=cr_env["CLOUDRU_ENDPOINT_URL"],
            aws_access_key_id=access_key,
            aws_secret_access_key=cr_env["CLOUDRU_SECRET_ACCESS_KEY"],
            config=Config(signature_version="s3v4", retries={"max_attempts": 5, "mode": "standard"}),
            region_name=cr_env.get("CLOUDRU_REGION", "ru-central-1"),
        )
        cr.put_object(
            Bucket=cr_env["CLOUDRU_BUCKET"],
            Key=ROOT_KEY,
            Body=body,
            ContentType="application/json; charset=utf-8",
            CacheControl="public, max-age=300",
        )
        public = cr_env.get("CLOUDRU_PUBLIC_URL", "").rstrip("/")
        print(f"  [Cloud.ru] OK → {public}/{ROOT_KEY}" if public else "  [Cloud.ru] OK")
    except Exception as e:
        print(f"  [Cloud.ru FAIL] {e!r}", file=sys.stderr)
        failures += 1

    print(f"\n[done] failures={failures}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
