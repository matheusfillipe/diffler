class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.1.10"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.10/diffler-v0.1.10-aarch64-apple-darwin.tar.gz"
      sha256 "133340c8000bb1ccff5d20e1c500d89aea68946241833485b9068da0cdaada54"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.10/diffler-v0.1.10-x86_64-apple-darwin.tar.gz"
      sha256 "37e944620a8f27a7e2d35f0a006c68e6ab1a4f043a012b4237cfd948ae8b294b"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.10/diffler-v0.1.10-aarch64-unknown-linux-musl.tar.gz"
      sha256 "4bf95c1ff12367f0f7129c656561f6b61764bb39f1bb76cf28619a137acb3b73"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.10/diffler-v0.1.10-x86_64-unknown-linux-musl.tar.gz"
      sha256 "f1e85be08c57102efafdd2edaa6d9c18cec0f8d2946d8af8fe638ab0a4a829fa"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
