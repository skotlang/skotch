; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [7 x i8] c"Kotlin\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)

define void @InputKt_greet(ptr %arg0) {
entry:
  call i32 @puts(ptr %arg0)
  ret void
}

define i32 @main() {
entry:
  call void @InputKt_greet(ptr @.str.0)
  ret i32 0
}

