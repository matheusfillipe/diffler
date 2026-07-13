class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.4.2"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.4.2/diffler-v0.4.2-aarch64-apple-darwin.tar.gz"
      sha256 "1386df847e5fb788ffb23b4620d3894fe304a77c6e26466876aa6c004b906614"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.4.2/diffler-v0.4.2-x86_64-apple-darwin.tar.gz"
      sha256 "734cb4cfd763f0cf2dbe3eb3d61cf1afe83d2e05e0a93891f470f8471aec31e1"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.4.2/diffler-v0.4.2-aarch64-unknown-linux-musl.tar.gz"
      sha256 "6345a2bd43d107239f0890ed0faa4eb5b19aac019aa36d2c8ab873eec79cf3dc"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.4.2/diffler-v0.4.2-x86_64-unknown-linux-musl.tar.gz"
      sha256 "fb4418e70d7404009725845816201117a7dcb79a52a3fb599709fed2e31bab68"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
