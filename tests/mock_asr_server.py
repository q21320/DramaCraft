"""极简的 OpenAI 兼容 ASR mock 服务，供端到端测试使用。

用法::

    python mock_asr_server.py <port> <record_file>

监听 127.0.0.1:<port>，把每次请求的形态（路径、鉴权头、Content-Type、
multipart 里有没有 file/model/response_format/language 字段）写进 <record_file>，
这样测试就能验证「我们到底发出去了什么」，而不是只验证「收到了什么」。
"""

import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1])
RECORD = sys.argv[2]


def record(entry: dict) -> None:
    try:
        with open(RECORD, "w", encoding="utf-8") as handle:
            json.dump(entry, handle, ensure_ascii=False)
    except Exception:
        pass


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self) -> None:  # noqa: N802 —— 标准库要求的方法名
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length) if length else b""
        text = body.decode("utf-8", "ignore")

        record(
            {
                "method": "POST",
                "path": self.path,
                "authorization": self.headers.get("Authorization", ""),
                "content_type": self.headers.get("Content-Type", ""),
                "body_bytes": len(body),
                "has_file_field": 'name="file"' in text,
                "has_model_field": 'name="model"' in text,
                "has_response_format": 'name="response_format"' in text,
                "has_language": 'name="language"' in text,
            }
        )

        if "/audio/transcriptions" not in self.path:
            self.send_json(404, {"error": "not found"})
            return

        if not self.headers.get("Authorization", "").startswith("Bearer "):
            self.send_json(401, {"error": {"message": "missing api key"}})
            return

        self.send_json(
            200,
            {
                "text": "你好世界",
                "language": "zh",
                "duration": 4.0,
                "segments": [
                    {"id": 0, "start": 0.2, "end": 2.0, "text": "你好"},
                    {"id": 1, "start": 2.2, "end": 3.8, "text": "世界"},
                ],
            },
        )

    def send_json(self, code: int, payload: dict) -> None:
        data = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args) -> None:  # 静音
        pass


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
