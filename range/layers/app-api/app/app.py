from flask import Flask, jsonify
app = Flask(__name__)

@app.route("/api/health")
def health():
    return jsonify(status="ok", db="connected", version="2.3.1")

@app.route("/api/users")
def users():
    return jsonify(users=["admin", "deploy", "backup"])

@app.route("/api/status")
def status():
    return jsonify({
        "status": "ok",
        "version": "1.0",
        "task_runner": "gateway-datacenter (10.99.3.10) /opt/tasks — drops executable files here for batch processing",
        "monitoring_ssh": "see /etc/monitoring/ssh.conf",
    })

if __name__ == "__main__":
    app.run(host="0.0.0.0", port=5000)
