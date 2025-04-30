# GitChronicler

GitChronicler is a tool that uses AI to automatically write or check your Git commit messages.

### Setup

An OpenRouter API key is required.  The OpenRouter API key is expected
   at the `~/.openrouter/key` path.
   ```
   mkdir -p ~/.openrouter
   echo "your-openrouter-api-key" > ~/.openrouter/key
   ```

## Usage

### Write a commit message

To write the commit message for the current diff, you can use:

```
git-chronicler write [-s] [--cached] [-i]
```

The `-s` flag will pass `-s` to the underlying `git commit` message,
while `--cached` will limit the commit to the staged files.

If `-i` is provided, then allow to customize the log message before commit.

### Check a commit message

To analyze the most recent commit and receive suggestions:

```
git-chronicler check
```

### Automatically improve a commit message

To replace the most recent commit message with an AI-improved version:
\
```
git-chronicler fixup
```

### Create a branch summary

To generate a summary of the changes in your current branch for use in pull request descriptions:

```
git-chronicler summary --base <base_branch>
```

This command analyzes all changes between your current HEAD and the specified base branch, creating a comprehensive summary suitable for pull request descriptions. The output includes:
- A one-line descriptive title suitable for the PR title
- A detailed description of the changes, with emphasis on commit messages

### git rebase -i

It is meant to be used interactively with `git rebase -i`.  To
rewrite/improve the git commit message for the current branch, you can
run:

```
git rebase -i $base_branch -x 'git-chronicler fixup'
```

If `fixup` is specified to `git-chronicler`, then the git commit
message is replaced inline and amended to the git patch.

## License

git-chronicler is licensed under the GNU General Public License v2.0 or later.