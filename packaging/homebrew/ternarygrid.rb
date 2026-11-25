# Homebrew cask for TernaryGrid.
# Publish the notarized DMG produced by scripts/bundle-macos.sh to the release page,
# then update `version` and `sha256` (printed by the script). Install it with:
#   brew install --cask ./packaging/homebrew/ternarygrid.rb
cask "ternarygrid" do
  version "0.7.9"
  sha256 "9cc62c354f07abf532ad2e40509b5ce3f52c8dd90497063fdca5e3142c52d1ed"

  url "https://github.com/ilhamnadhif/ternarygrid/releases/download/v#{version}/TernaryGrid-#{version}.dmg"
  name "TernaryGrid"
  desc "Lightweight SQL database client written in Rust"
  homepage "https://github.com/ilhamnadhif/ternarygrid"

  depends_on macos: ">= :monterey"

  app "TernaryGrid.app"
  binary "#{appdir}/TernaryGrid.app/Contents/MacOS/ternarygrid"

  zap trash: [
    "~/Library/Application Support/dev.ternarygrid.TernaryGrid",
    "~/Library/Caches/dev.ternarygrid.TernaryGrid",
    "~/Library/Preferences/dev.ternarygrid.TernaryGrid.plist",
  ]
end
