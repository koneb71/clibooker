# bk shell integration for zsh — load with:  eval "$(bk init zsh)"
#
# - remembers the last command you ran (with its folder and active env) for `bk save`
# - `bk go NAME` enters a bookmark's folder + env in the current shell
# - tab completion for bookmark names
if [[ -z ${__BK_ZSH_LOADED-} ]]; then
  typeset -g __BK_ZSH_LOADED=1
  typeset -g __BK_RECORD=__BK_RECORD_VALUE__
  typeset -g __bk_pending="" __bk_pending_x="" __bk_pending_pwd="" __bk_pending_conda="" __bk_pending_venv=""
  typeset -g __bk_last_cmd="" __bk_last_cmd_x="" __bk_last_pwd="" __bk_last_status="" __bk_last_conda="" __bk_last_venv=""
  zmodload -F zsh/datetime p:EPOCHSECONDS 2>/dev/null
  autoload -Uz add-zsh-hook

  __bk_preexec() {
    __bk_pending=${1:-$3}
    __bk_pending_x=$3
    __bk_pending_pwd=$PWD
    __bk_pending_conda=${CONDA_DEFAULT_ENV-}
    __bk_pending_venv=${VIRTUAL_ENV-}
  }

  __bk_record() {
    emulate -L zsh
    setopt extendedglob
    local cmd=$__bk_pending
    [[ $cmd == ' '* ]] && return
    [[ $cmd == (#i)*(password|passwd|secret|token|api[_-]#key|bearer|authorization)* ]] && return
    print -rn -- "${EPOCHSECONDS:-0}"$'\x1f'"$1"$'\x1f'"$__bk_pending_pwd"$'\x1f'"$__bk_pending_conda"$'\x1f'"$__bk_pending_venv"$'\x1f'"$cmd"$'\x1e\n' >>| $__BK_RECENT_FILE
  }

  __bk_precmd() {
    local st=$?
    [[ -z $__bk_pending ]] && return
    local w first=""
    for w in ${(z)__bk_pending}; do
      [[ $w == *=* || $w == (command|builtin|noglob|nocorrect|exec) ]] && continue
      first=$w
      break
    done
    if [[ $first != bk && $first != */bk ]]; then
      __bk_last_cmd=$__bk_pending
      __bk_last_cmd_x=$__bk_pending_x
      __bk_last_pwd=$__bk_pending_pwd
      __bk_last_status=$st
      __bk_last_conda=$__bk_pending_conda
      __bk_last_venv=$__bk_pending_venv
      (( __BK_RECORD )) && __bk_record $st
    fi
    __bk_pending=""
  }

  if (( __BK_RECORD )); then
    typeset -g __BK_RECENT_FILE=__BK_RECENT_FILE__
    [[ -e $__BK_RECENT_FILE ]] || ( umask 077; mkdir -p ${__BK_RECENT_FILE:h} && : >> $__BK_RECENT_FILE )
  fi

  add-zsh-hook preexec __bk_preexec
  # Run first so $? is still the command's status.
  precmd_functions=(__bk_precmd ${precmd_functions:#__bk_precmd})

  bk() {
    if [[ $1 == go ]]; then
      shift
      local __bk_out
      __bk_out=$(command bk go --print "$@") || return
      eval "$__bk_out"
      return
    fi
    local kind=""
    if [[ $1 == save && -n $__bk_last_cmd ]]; then
      local -a __bk_words=(${(z)__bk_last_cmd})
      kind=$(whence -w -- ${__bk_words[1]} 2>/dev/null)
      kind=${kind#*: }
    fi
    BK_SHELL_INTEGRATION=zsh \
    BK_LAST_CMD=$__bk_last_cmd BK_LAST_CMD_EXPANDED=$__bk_last_cmd_x BK_LAST_PWD=$__bk_last_pwd \
    BK_LAST_STATUS=$__bk_last_status BK_LAST_CONDA=$__bk_last_conda BK_LAST_VENV=$__bk_last_venv \
    BK_LAST_KIND=$kind command bk "$@"
  }

  _bk() {
    local -a subcmds names
    subcmds=(
      'add:save a new bookmark' 'save:bookmark the command you just ran' 'run:run a bookmark'
      'ls:list bookmarks' 'show:show details' 'edit:edit in $EDITOR' 'rm:delete' 'mv:rename'
      'go:enter its folder and env' 'history:past runs' 'doctor:check bookmarks' 'trust:approve a project .bk.toml'
      'export:export bookmarks' 'import:import bookmarks' 'init:print shell integration' 'app:open the desktop app'
      'scan:bookmark a project'"'"'s scripts' 'server:manage SSH servers' 'servers:list SSH servers' 'ssh:connect to a server'
    )
    if (( CURRENT == 2 )); then
      names=(${(f)"$(command bk ls --names 2>/dev/null)"})
      _describe -t bookmarks 'bookmark' names
      _describe -t commands 'bk command' subcmds
    elif (( CURRENT == 3 )) && [[ ${words[2]} == (run|show|rm|remove|mv|rename|go|edit|history|export) ]]; then
      names=(${(f)"$(command bk ls --names 2>/dev/null)"})
      _describe -t bookmarks 'bookmark' names
    else
      _files
    fi
  }
  (( $+functions[compdef] )) && compdef _bk bk
fi
