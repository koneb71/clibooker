# bk shell integration for bash — load with:  eval "$(bk init bash)"
#
# - remembers the last command you ran (with its folder and active env) for `bk save`
# - `bk go NAME` enters a bookmark's folder + env in the current shell
# - tab completion for bookmark names
#
# No DEBUG trap (it would fight starship, atuin, VS Code and bash-preexec): the folder and env
# are snapshotted at each prompt, which is exactly the state the next command starts in, and
# a new command is detected by its history number.
if [[ -z ${__BK_BASH_LOADED-} ]]; then
  __BK_BASH_LOADED=1
  __BK_RECORD=__BK_RECORD_VALUE__
  __BK_RECENT_FILE=__BK_RECENT_FILE__
  __bk_histnum=-1
  __bk_snap_pwd=$PWD __bk_snap_conda=${CONDA_DEFAULT_ENV-} __bk_snap_venv=${VIRTUAL_ENV-}
  __bk_last_cmd="" __bk_last_pwd="" __bk_last_status="" __bk_last_conda="" __bk_last_venv=""

  __bk_record() {
    local st=$1 cmd=$2 ts=${EPOCHSECONDS-} nc=""
    [[ $cmd == ' '* ]] && return
    shopt -q nocasematch && nc=1
    shopt -s nocasematch
    if [[ $cmd =~ (password|passwd|secret|token|api[_-]*key|bearer|authorization) ]]; then
      [[ -z $nc ]] && shopt -u nocasematch
      return
    fi
    [[ -z $nc ]] && shopt -u nocasematch
    [[ -z $ts ]] && ts=$(date +%s)
    printf '%s\037%s\037%s\037%s\037%s\037%s\036\n' "$ts" "$st" "$__bk_snap_pwd" "$__bk_snap_conda" \
      "$__bk_snap_venv" "$cmd" >> "$__BK_RECENT_FILE"
  }

  __bk_precmd() {
    local st=$? line num cmd w first=""
    local -a words
    line=$(HISTTIMEFORMAT= builtin history 1 2>/dev/null)
    # `history` prints "%5d%c %s": the number, `*` or a space, one space, then the line as typed.
    if [[ $line =~ ^[[:space:]]*([0-9]+)[*[:space:]][[:space:]](.*)$ ]]; then
      num=${BASH_REMATCH[1]}
      cmd=${BASH_REMATCH[2]}
      if [[ $__bk_histnum == -1 ]]; then
        # First prompt: the newest history entry is from an earlier session.
        __bk_histnum=$num
      elif [[ $num != "$__bk_histnum" ]]; then
        __bk_histnum=$num
        read -ra words <<< "$cmd"
        for w in "${words[@]}"; do
          [[ $w == *=* || $w == command || $w == builtin || $w == exec || $w == nohup ]] && continue
          first=$w
          break
        done
        if [[ $first != bk && $first != */bk ]]; then
          __bk_last_cmd=$cmd
          __bk_last_pwd=$__bk_snap_pwd
          __bk_last_status=$st
          __bk_last_conda=$__bk_snap_conda
          __bk_last_venv=$__bk_snap_venv
          (( __BK_RECORD )) && __bk_record "$st" "$cmd"
        fi
      fi
    fi
    __bk_snap_pwd=$PWD __bk_snap_conda=${CONDA_DEFAULT_ENV-} __bk_snap_venv=${VIRTUAL_ENV-}
    # Later prompt hooks still see the command's status.
    return $st
  }

  if (( __BK_RECORD )) && [[ ! -e $__BK_RECENT_FILE ]]; then
    ( umask 077; mkdir -p "${__BK_RECENT_FILE%/*}" && : >> "$__BK_RECENT_FILE" )
  fi

  if [[ -n ${bash_preexec_imported-}${__bp_imported-} ]]; then
    precmd_functions=(__bk_precmd "${precmd_functions[@]}")
  elif [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == "declare -a"* ]]; then
    PROMPT_COMMAND=(__bk_precmd "${PROMPT_COMMAND[@]}")
  else
    PROMPT_COMMAND="__bk_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
  fi

  bk() {
    if [[ ${1-} == go ]]; then
      shift
      local __bk_out
      __bk_out=$(command bk go --print "$@") || return
      eval "$__bk_out"
      return
    fi
    local kind="" expanded="" first rest def
    if [[ ${1-} == save && -n $__bk_last_cmd ]]; then
      first=${__bk_last_cmd%%[[:space:]]*}
      rest=${__bk_last_cmd#"$first"}
      kind=$(type -t -- "$first" 2>/dev/null)
      if [[ $kind == alias ]]; then
        def=$(alias -- "$first" 2>/dev/null)
        eval "def=${def#*=}"
        expanded="$def$rest"
      fi
    fi
    BK_SHELL_INTEGRATION=bash \
    BK_LAST_CMD=$__bk_last_cmd BK_LAST_CMD_EXPANDED=$expanded BK_LAST_PWD=$__bk_last_pwd \
    BK_LAST_STATUS=$__bk_last_status BK_LAST_CONDA=$__bk_last_conda BK_LAST_VENV=$__bk_last_venv \
    BK_LAST_KIND=$kind command bk "$@"
  }

  __bk_complete() {
    local cur=${COMP_WORDS[COMP_CWORD]}
    local subcmds="add save run ls show edit rm mv go history doctor trust export import init app scan server servers ssh sync"
    if (( COMP_CWORD == 1 )); then
      COMPREPLY=($(compgen -W "$(command bk ls --names 2>/dev/null) $subcmds" -- "$cur"))
    elif (( COMP_CWORD == 2 )) && [[ ${COMP_WORDS[1]} =~ ^(run|show|rm|remove|mv|rename|go|edit|history|export)$ ]]; then
      COMPREPLY=($(compgen -W "$(command bk ls --names 2>/dev/null)" -- "$cur"))
    else
      COMPREPLY=()
    fi
  }
  complete -o default -F __bk_complete bk
fi
