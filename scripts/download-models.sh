#!/usr/bin/env bash
# Download face-auth ONNX models from HuggingFace
# Models are too large to store in git.
set -euo pipefail

MODELS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/models"
mkdir -p "$MODELS_DIR"

# Model sources (HuggingFace)
# SCRFD-500M: InsightFace detection model (~2.5 MB)
DET_URL="https://huggingface.co/deepinsight/insightface/resolve/main/models/buffalo_s/det_500m.onnx"
# ArcFace MobileFaceNet w600k: recognition model (~14 MB)
REC_URL="https://huggingface.co/deepinsight/insightface/resolve/main/models/buffalo_l/w600k_mbf.onnx"

download() {
    local url="$1"
    local dest="$2"
    local name
    name="$(basename "$dest")"

    if [[ -f "$dest" ]]; then
        echo "  [skip] $name already exists"
        return
    fi

    echo "  [download] $name ..."
    if command -v curl &>/dev/null; then
        curl -L --progress-bar -o "$dest" "$url"
    elif command -v wget &>/dev/null; then
        wget -q --show-progress -O "$dest" "$url"
    else
        echo "ERROR: neither curl nor wget found" >&2
        exit 1
    fi
    echo "  [ok] $name"
}

echo "Downloading face-auth models to: $MODELS_DIR"
echo

download "$DET_URL" "$MODELS_DIR/det_500m.onnx"
download "$REC_URL" "$MODELS_DIR/w600k_mbf.onnx"

echo
echo "Models ready. Verify with:"
echo "  face-enroll --check-config"
