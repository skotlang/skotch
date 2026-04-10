; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [14 x i8] c"Hello, world!\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)

define i32 @main() {
entry:
  call i32 @puts(ptr @.str.0)
  ret i32 0
}

