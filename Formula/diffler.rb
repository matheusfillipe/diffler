class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.1.14"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.14/diffler-v0.1.14-aarch64-apple-darwin.tar.gz"
      sha256 "98263b6883f1c1f07a01cc08e4538a84fa5025938b7bb000dce52484ed5189e4"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.14/diffler-v0.1.14-x86_64-apple-darwin.tar.gz"
      sha256 "057a188cd433815c25b878e51ecc59b149b83b6e6d28d7d09476a16e72a2875a"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.14/diffler-v0.1.14-aarch64-unknown-linux-musl.tar.gz"
      sha256 "f48f8187a595cbae899b96b338938fc7504329ab1b649e71f509b386ae88c0cd"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.14/diffler-v0.1.14-x86_64-unknown-linux-musl.tar.gz"
      sha256 "23209cbf0da129274c3088a9c65ef65d5ffa4eeabd7f1b5540fbfa908e1defd1"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
