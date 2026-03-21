from flask import Flask, request, jsonify
from FlagEmbedding import FlagReranker
import os, torch

app = Flask(__name__)
reranker = None

# ── Configurable via environment variables ─────────────────────────
RERANKER_MODEL = os.environ.get("RERANKER_MODEL", "BAAI/bge-reranker-v2-m3")
RERANKER_DEVICE = os.environ.get("RERANKER_DEVICE", "cuda" if torch.cuda.is_available() else "cpu")
RERANKER_PORT = int(os.environ.get("RERANKER_PORT", "8678"))

def get_reranker():
    global reranker
    if reranker is None:
        reranker = FlagReranker(RERANKER_MODEL, use_fp16=True, device=RERANKER_DEVICE)
    return reranker

@app.route('/rerank', methods=['POST'])
def rerank():
    data = request.json
    query = data.get('query', '')
    documents = data.get('documents', [])
    top_k = data.get('top_k', len(documents))

    if not query or not documents:
        return jsonify({'error': 'query and documents required'}), 400

    pairs = [[query, doc] for doc in documents]
    scores = get_reranker().compute_score(pairs)
    if isinstance(scores, float):
        scores = [scores]

    results = sorted(
        [{'index': i, 'document': doc, 'score': score}
         for i, (doc, score) in enumerate(zip(documents, scores))],
        key=lambda x: x['score'], reverse=True
    )[:top_k]

    return jsonify({'results': results})

@app.route('/health', methods=['GET'])
def health():
    return jsonify({'status': 'ok', 'model': RERANKER_MODEL, 'device': RERANKER_DEVICE})

if __name__ == '__main__':
    print(f'Loading reranker model ({RERANKER_MODEL}) on {RERANKER_DEVICE}...')
    get_reranker()
    print(f'Reranker server ready on :{RERANKER_PORT}')
    app.run(host='0.0.0.0', port=RERANKER_PORT)
