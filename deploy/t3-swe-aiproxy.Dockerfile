# Live in-TDX SWE-bench evaluation with host-metered playbox-aiproxy (T3 + source: "host_aiproxy").
# Build context: repository root.
FROM python:3.14-slim-trixie

ENV DEBIAN_FRONTEND=noninteractive \
    PYTHONUNBUFFERED=1 \
    HERMES_DISABLE_LAZY_INSTALLS=1 \
    PATH="/opt/hermes-venv/bin:${PATH}" \
    PLAYBOX_BIN=/usr/local/bin/playbox \
    PB_HERMES_PY=/opt/hermes-venv/bin/python \
    PB_HERMES_AGENT_DIR=/opt/hermes-agent \
    PB_SWE_DIR=/opt/playbox/swe-slice

RUN apt-get update && apt-get install -y --no-install-recommends \
      curl ca-certificates git openssh-client build-essential ripgrep jq procps && \
    rm -rf /var/lib/apt/lists/* && \
    pip3 install --no-cache-dir cryptography httpx pydantic pyyaml

RUN git clone --depth 1 https://github.com/NousResearch/hermes-agent.git /opt/hermes-agent && \
    pip3 install --no-cache-dir uv && \
    UV_PROJECT_ENVIRONMENT=/opt/hermes-venv uv sync --project /opt/hermes-agent --no-dev && \
    uv pip install --python /opt/hermes-venv/bin/python cryptography httpx pydantic pyyaml

RUN mkdir -p /root/.hermes /root/.ssh && chmod 700 /root/.ssh && \
    printf 'model:\n  default: gemini-3.1-pro-preview\n  provider: custom\nterminal:\n  backend: playbox\n  cwd: /sandbox\nplugins:\n  enabled: []\n' \
      > /root/.hermes/config.yaml

COPY target/playbox-linux-amd64 /usr/local/bin/playbox
COPY target/playbox-aiproxy-linux-amd64 /usr/local/bin/playbox-aiproxy
COPY deploy/t3-swe-aiproxy-entrypoint.sh /usr/local/bin/t3-swe-aiproxy-entrypoint.sh
COPY docs/reports/google-confidential-space-jwks.json /opt/playbox/google-confidential-space-jwks.json
COPY hermes-playbox-backend /opt/playbox/hermes-playbox-backend
COPY tools/bench/hermes-parity /opt/playbox/tools/bench/hermes-parity
COPY tools/bench/hermes-parity-playbox /opt/playbox/tools/bench/hermes-parity-playbox
COPY tools/bench/tripwire /opt/playbox/tools/bench/tripwire
COPY tools/playbox_watch.py /opt/playbox/tools/playbox_watch.py
COPY tmp-tdx-swe-slice /opt/playbox/swe-slice

RUN chmod +x /usr/local/bin/playbox /usr/local/bin/playbox-aiproxy /usr/local/bin/t3-swe-aiproxy-entrypoint.sh

LABEL "tee.launch_policy.allow_env_override"="PLAYBOX_CS_AUDIENCE,PLAYBOX_CS_PROJECTS,PLAYBOX_CS_IMAGES,GCP_PROJECT,GCS_BUCKET,GCS_OUT_PREFIX,SWE_CONCURRENCY,SWE_TIMEOUT,SWE_T1_MODEL,SWE_T2_MODEL,SWE_MAX_TIERS,REMOTE_GRADER_SSH,SSH_KEY_B64,UPSTREAM_BASE_URL,UPSTREAM_API_KEY,UPSTREAM_INPUT_PRICE,UPSTREAM_OUTPUT_PRICE"
LABEL "tee.launch_policy.log_redirect"="always"

ENTRYPOINT ["/usr/local/bin/t3-swe-aiproxy-entrypoint.sh"]
