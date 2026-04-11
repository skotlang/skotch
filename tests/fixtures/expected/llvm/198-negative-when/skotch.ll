; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [11 x i8] c"minus five\00", align 1
@.str.1 = private unnamed_addr constant [5 x i8] c"zero\00", align 1
@.str.2 = private unnamed_addr constant [5 x i8] c"five\00", align 1
@.str.3 = private unnamed_addr constant [6 x i8] c"other\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)

define i32 @main() {
entry:
  %merge_3 = alloca ptr
  %t0 = add i32 0, -5
  br label %bb1
bb1:
  %t1 = add i32 0, -5
  %t2 = icmp eq i32 %t0, %t1
  br i1 %t2, label %bb2, label %bb3
bb2:
  store ptr @.str.0, ptr %merge_3
  br label %bb8
bb3:
  %t3 = add i32 0, 0
  %t4 = icmp eq i32 %t0, %t3
  br i1 %t4, label %bb4, label %bb5
bb4:
  store ptr @.str.1, ptr %merge_3
  br label %bb8
bb5:
  %t5 = add i32 0, 5
  %t6 = icmp eq i32 %t0, %t5
  br i1 %t6, label %bb6, label %bb7
bb6:
  store ptr @.str.2, ptr %merge_3
  br label %bb8
bb7:
  store ptr @.str.3, ptr %merge_3
  br label %bb8
bb8:
  %t7 = load ptr, ptr %merge_3
  call i32 @puts(ptr %t7)
  ret i32 0
}

