# Homebrew cask for TernaryGrid.
# Publish the notarized DMG produced by scripts/bundle-macos.sh to the release page,
# then update `version` and `sha256` (printed by the script). Install it with:
#   brew install --cask ./packaging/homebrew/ternarygrid.rb
cask "ternarygrid" do
  version "0.6.1"
  sha256 "24de4e497e01261fc58da208be3f53e973c593fc5b1f3a0507ccb1fd466aa4a4"

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
