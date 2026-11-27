class FastDocs < Formula
  desc "Blazing-fast MCP documentation server for AI coding assistants"
  homepage "https://github.com/bepi64/homebrew-tap"
  url "https://github.com/bepi64/homebrew-tap/archive/refs/tags/v1.0.0.tar.gz"
  # Replace with the real archive SHA-256 once tag v1.0.0 is published on GitHub.
  sha256 "956185d42882f8fcae9cc88bedcc49a0ab1fc806cb7f8604cc57f1371817f796"
  license "Apache-2.0"

  depends_on "rust" => :build

  on_macos do
    on_arm do
      resource "tesseract" do
        url "https://github.com/bepi64/homebrew-tap/releases/download/tesseract-5.5.5/fast-docs-tesseract-5.5.5-aarch64-apple-darwin.tar.gz"
        # Replace with the real archive SHA-256 once tag tesseract-5.5.5 is published on GitHub.
        sha256 "54b1b9c01f60d87f97d3bc4cbc66710d483153e5c28a5000570bed527429db29"
      end
    end
  end

  def install
    if OS.mac? && Hardware::CPU.arm?
      resource("tesseract").stage do
        (libexec/"lib").install Dir["lib/*.dylib"]
        (libexec/"tessdata").install Dir["tessdata/*.traineddata"]
      end
      ENV["FAST_DOCS_TESSERACT_LIB_DIR"] = (libexec/"lib").to_s
      ENV["FAST_DOCS_PORTABLE_RPATH"] = "1"
      system "cargo", "install", "--features", "ocr", *std_cargo_args
    else
      system "cargo", "install", *std_cargo_args
    end
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

    if OS.mac? && Hardware::CPU.arm?
      port = free_port
      pid = spawn bin/"fast-docs", "--http", "127.0.0.1:#{port}"
      begin
        sleep 2
        health = shell_output("curl -s http://127.0.0.1:#{port}/health")
        assert_match '"engine":"tesseract"', health
        assert_match '"tessdata":"ok"', health
      ensure
        Process.kill("TERM", pid)
        Process.wait(pid)
      end
    end
  end
end
