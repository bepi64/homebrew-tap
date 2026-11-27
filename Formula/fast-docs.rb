class FastDocs < Formula
  desc "Blazing-fast MCP documentation server for AI coding assistants"
  homepage "https://github.com/bepi64/homebrew-tap"
  url "https://github.com/bepi64/homebrew-tap/archive/refs/tags/v1.0.0.tar.gz"
  # Replace with the real archive SHA-256 once tag v1.0.0 is published on GitHub.
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license "Apache-2.0"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  service do
    run [opt_bin/"fast-docs", "--http", "127.0.0.1:3000"]
    keep_alive true
    log_path var/"log/fast-docs.log"
    error_log_path var/"log/fast-docs.log"
  end

  test do
    init_req = {
      jsonrpc: "2.0",
      id:      1,
      method:  "initialize",
      params:  {
        protocolVersion: "2024-11-05",
        capabilities:    {},
        clientInfo:      { name: "brew-test", version: "1.0.0" },
      },
    }.to_json
    initialized_note = {
      jsonrpc: "2.0",
      method:  "notifications/initialized",
    }.to_json

    output = pipe_output("#{bin}/fast-docs --stdio", "#{init_req}\n#{initialized_note}\n", 0)
    assert_match "fast-docs", output
  end
end
