class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.13.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.13.0/diffler-v0.13.0-aarch64-apple-darwin.tar.gz"
      sha256 "b17b518dc6ecbf1b3642d74b3739d6468e909d685efcaf722f4ed737595abe05"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.13.0/diffler-v0.13.0-x86_64-apple-darwin.tar.gz"
      sha256 "b3c4feb01e5563ee600b6eb75d929a5fbf98f7f1697217d32aabba36fd407278"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.13.0/diffler-v0.13.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "8ded159ff7837a2faac396b4a45b1bc243317b3a6bb5b7b1bc0d22f3820f4fa9"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.13.0/diffler-v0.13.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "9804a7009ae197426207063bf0727e2ece03661b7e8cfdac0ecd076f3a0252b2"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
