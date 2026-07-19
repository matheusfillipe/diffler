class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.6.2"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.2/diffler-v0.6.2-aarch64-apple-darwin.tar.gz"
      sha256 "51f4ec1c35d0768646ae144d61de27b5072da9f310e231f788f16e34e37662cd"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.2/diffler-v0.6.2-x86_64-apple-darwin.tar.gz"
      sha256 "14b858358a8e8e78f9f796e7eb35950177a56a26b0acdd4fe81ba92636282cbc"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.2/diffler-v0.6.2-aarch64-unknown-linux-musl.tar.gz"
      sha256 "7ab0196522e832720696db5b802bd1321152d8b94f62c5a67741c6a46fed2b7b"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.2/diffler-v0.6.2-x86_64-unknown-linux-musl.tar.gz"
      sha256 "a9ef45215670fab55d608443bda21cfccd417dc4c95a6874a950125095e2bcf8"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
