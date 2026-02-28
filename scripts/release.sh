#!/bin/bash
# Interactive release script for ccs (claude-code-sync)

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

# Get project directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_DIR"

# Get current version
CURRENT_VERSION=$(grep '^version = ' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT_VERSION"

# Calculate next versions
NEXT_PATCH="${MAJOR}.${MINOR}.$((PATCH + 1))"
NEXT_MINOR="${MAJOR}.$((MINOR + 1)).0"
NEXT_MAJOR="$((MAJOR + 1)).0.0"

# Show header
echo ""
echo -e "${BOLD}╔════════════════════════════════════════╗${NC}"
echo -e "${BOLD}║          ccs Release Tool              ║${NC}"
echo -e "${BOLD}╚════════════════════════════════════════╝${NC}"
echo ""
echo -e "Current version: ${GREEN}v${CURRENT_VERSION}${NC}"
echo ""

# Show menu
echo -e "${BOLD}Select an action:${NC}"
echo ""
echo -e "  ${CYAN}1)${NC} Push only         - Push commits to remote (no version change)"
echo -e "  ${CYAN}2)${NC} Release patch     - ${YELLOW}v${CURRENT_VERSION}${NC} → ${GREEN}v${NEXT_PATCH}${NC}  (bug fixes)"
echo -e "  ${CYAN}3)${NC} Release minor     - ${YELLOW}v${CURRENT_VERSION}${NC} → ${GREEN}v${NEXT_MINOR}${NC}  (new features)"
echo -e "  ${CYAN}4)${NC} Release major     - ${YELLOW}v${CURRENT_VERSION}${NC} → ${GREEN}v${NEXT_MAJOR}${NC}  (breaking changes)"
echo -e "  ${CYAN}q)${NC} Quit"
echo ""

read -p "Your choice [1-4/q]: " choice

case "$choice" in
    1)
        ACTION="push"
        ;;
    2)
        ACTION="patch"
        NEW_VERSION="$NEXT_PATCH"
        ;;
    3)
        ACTION="minor"
        NEW_VERSION="$NEXT_MINOR"
        ;;
    4)
        ACTION="major"
        NEW_VERSION="$NEXT_MAJOR"
        ;;
    q|Q)
        echo -e "${YELLOW}Cancelled.${NC}"
        exit 0
        ;;
    *)
        echo -e "${RED}Invalid choice.${NC}"
        exit 1
        ;;
esac

echo ""

# Check for uncommitted changes
HAS_CHANGES=false
if ! git diff --quiet || ! git diff --staged --quiet || [ -n "$(git ls-files --others --exclude-standard)" ]; then
    HAS_CHANGES=true
    echo -e "${YELLOW}Uncommitted changes detected:${NC}"
    git status --short
    echo ""
fi

if [ "$ACTION" = "push" ]; then
    # Push only
    if [ "$HAS_CHANGES" = true ]; then
        read -p "Commit all changes before push? [Y/n]: " -n 1 -r
        echo ""
        if [[ ! $REPLY =~ ^[Nn]$ ]]; then
            read -p "Commit message: " COMMIT_MSG
            if [ -z "$COMMIT_MSG" ]; then
                COMMIT_MSG="chore: update"
            fi
            git add -A
            git commit -m "$COMMIT_MSG"
        fi
    fi
    echo -e "${CYAN}Pushing to remote...${NC}"
    git push origin HEAD
    echo ""
    echo -e "${GREEN}✓ Push complete!${NC}"
else
    # Release with version bump
    TAG_NAME="v${NEW_VERSION}"

    # Check if tag exists
    if git rev-parse "$TAG_NAME" >/dev/null 2>&1; then
        echo -e "${RED}Error: Tag ${TAG_NAME} already exists!${NC}"
        exit 1
    fi

    # Stage all changes first if any
    if [ "$HAS_CHANGES" = true ]; then
        echo -e "${CYAN}Staging all changes...${NC}"
        git add -A
    fi

    echo -e "${CYAN}Updating version to ${NEW_VERSION}...${NC}"
    sed -i '' "s/^version = \"${CURRENT_VERSION}\"/version = \"${NEW_VERSION}\"/" Cargo.toml
    git add Cargo.toml

    echo -e "${CYAN}Committing...${NC}"
    git commit -m "chore: bump version to ${NEW_VERSION}"

    echo -e "${CYAN}Creating tag ${TAG_NAME}...${NC}"
    git tag -a "$TAG_NAME" -m "Release ${TAG_NAME}"

    echo -e "${CYAN}Pushing...${NC}"
    git push origin HEAD
    git push origin "$TAG_NAME"

    echo ""
    echo -e "${GREEN}✓ Release ${TAG_NAME} complete!${NC}"
    echo ""
    echo -e "GitHub Actions: ${CYAN}https://github.com/osen77/claude-code-sync-cn/actions${NC}"
fi
