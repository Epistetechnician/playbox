"""Local unit + mock-server tests for LoCoMo and FinanceBench SOTA adapters."""
from __future__ import annotations

import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

from playbox_steer.kv_sota_bench import (
    BM25ChunkRetriever,
    SotaEntityRouter,
    load_financebench,
    load_locomo,
    run_sota_cell,
    score_financebench_turn,
    score_locomo_turn,
)


def _start_mock_server():
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, format, *args):
            return

        def do_GET(self):
            if self.path == "/metrics":
                body = (
                    "vllm:prefix_cache_queries_total 100\n"
                    "vllm:prefix_cache_hits_total 80\n"
                ).encode()
                self.send_response(200)
                self.end_headers()
                self.wfile.write(body)

        def do_POST(self):
            ln = int(self.headers.get("Content-Length", "0"))
            req = json.loads(self.rfile.read(ln).decode())
            msgs = req["messages"]
            sys_c = msgs[0]["content"]
            usr_c = msgs[-1]["content"]
            if "Rewrite the follow-up" in sys_c:
                text = "When did Caroline attend the support group?"
            elif "7 May 2023" in sys_c or "7 May 2023" in usr_c:
                text = "Answer: 7 May 2023"
            elif "$1577.00" in sys_c or "1577" in sys_c or "1577" in usr_c:
                text = "Answer: $1577 million"
            else:
                text = "Answer: Yes"

            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            chunk1 = {"choices": [{"delta": {"content": text}, "finish_reason": None}]}
            chunk2 = {
                "choices": [{"delta": {}, "finish_reason": "stop"}],
                "usage": {
                    "prompt_tokens": max(10, len(sys_c) // 4),
                    "completion_tokens": 6,
                    "prompt_tokens_details": {"cached_tokens": max(0, len(sys_c) // 5)},
                },
            }
            self.wfile.write(f"data: {json.dumps(chunk1)}\n\n".encode())
            self.wfile.write(f"data: {json.dumps(chunk2)}\n\n".encode())
            self.wfile.write(b"data: [DONE]\n\n")

    srv = HTTPServer(("127.0.0.1", 0), Handler)
    port = srv.server_address[1]
    th = threading.Thread(target=srv.serve_forever, daemon=True)
    th.start()
    return srv, f"http://127.0.0.1:{port}"


def test_scorers():
    f1, hit = score_locomo_turn("Caroline went on 7 May 2023.\nAnswer: 7 May 2023", "7 May 2023")
    assert f1 == 1.0 and hit == 1
    f1_fb, hit_fb = score_financebench_turn("Answer: $1,577 million", "$1577.00")
    assert hit_fb == 1 and f1_fb == 1.0
    _, hit_yn = score_financebench_turn("Answer: Yes, it is capital-intensive.", "Yes.")
    assert hit_yn == 1


def test_locomo_and_financebench_e2e(tmp_path: Path):
    locomo_file = tmp_path / "locomo.json"
    locomo_file.write_text(
        json.dumps(
            [
                {
                    "conversation": {
                        "speaker_a": "Caroline",
                        "speaker_b": "Melanie",
                        "session_1_date_time": "7 May 2023",
                        "session_1": [
                            {"speaker": "Caroline", "text": "I went to the LGBTQ support group on 7 May 2023."}
                        ],
                    },
                    "qa": [
                        {"question": "When did Caroline go to the LGBTQ support group?", "answer": "7 May 2023", "category": 2},
                        {"question": "Who went with her?", "answer": "7 May 2023", "category": 1},
                    ],
                }
            ]
        )
    )
    fb_file = tmp_path / "fb.jsonl"
    fb_file.write_text(
        json.dumps(
            {
                "financebench_id": "fb_001",
                "company": "3M",
                "doc_name": "3M_2018_10K",
                "question_type": "metrics-generated",
                "question": "What is FY2018 capex for 3M?",
                "answer": "$1577.00",
                "evidence": [{"doc_name": "3M_2018_10K", "evidence_page_num": 50, "evidence_text_full_page": "3M FY2018 capex was $1577 million."}],
            }
        )
        + "\n"
    )

    loc = load_locomo(locomo_file, max_per_conv=4, turns_per_session=2)
    fb = load_financebench(fb_file)
    srv, url = _start_mock_server()
    try:
        for arm in ("S-RAG", "S-CONV-RAG", "S-ROUTED"):
            res = run_sota_cell(loc, arm, 1, 1, url, "mock", BM25ChunkRetriever(loc.chunks, top_k=2))
            assert res["summary"]["errors"] == 0
            assert res["summary"]["accuracy"] == 1.0
        for arm in ("S-CTX", "S-RAG", "S-ROUTED"):
            res = run_sota_cell(fb, arm, 1, 1, url, "mock", BM25ChunkRetriever(fb.chunks, top_k=2))
            assert res["summary"]["errors"] == 0
            assert res["summary"]["accuracy"] == 1.0
    finally:
        srv.shutdown()
