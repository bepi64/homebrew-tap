class FastDocs < Formula
  desc "Blazing-fast documentation fetcher for AI coding assistants"
  homepage "https://github.com/bepi64/homebrew-tap"
  url "https://github.com/bepi64/homebrew-tap.git",
      tag: "v0.1.0"
  license "Apache-2.0"

  depends_on "rust" => :build

  resource "libgit2-plugin" do
    url "https://github.com/bepi64/homebrew-tap/releases/download/v0.2.0/plugin-darwin-arm64.bundle"
    sha256 "cd5a06a8193efc4c4df46c7216e35c6dfbcb2193efdecb7ff1d6f34de243916e"
  end

  def install
    resource("libgit2-plugin").stage do
      (lib/"fast-docs").install "plugin-darwin-arm64.bundle" => "libgit2.dylib"
    end

    ENV["FAST_DOCS_LIB_DIR"] = "#{lib}/fast-docs"
    system "cargo", "build", "--release"
    bin.install "target/release/fast-docs"
  end

  service do
    run [opt_bin/"fast-docs"]
    keep_alive true
    log_path "/dev/null"
    error_log_path "/dev/null"
  end

  test do
    assert_predicate bin/"fast-docs", :exist?
  end
end
